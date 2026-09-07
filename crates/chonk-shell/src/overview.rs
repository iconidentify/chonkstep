//! Overview session state and input routing. Native compositors receive window
//! transforms and cached, small captions: the output-sized shell is input-only.
//! Hover changes the selected index without rasterizing, capturing or resizing
//! a window. Closing drops the scene and captions while preserving shell IDs.
//!
//! Noncompositing backends retain the raster fallback: a full panel on entry,
//! a separate selection surface, and one catch-up fetch for sharper previews.

use wm_core::{Backend, ClientId, DragHandle, OverviewDrag};
use wm_theme::overview::{self as ov, OverviewEntry, OverviewLayout};
use wm_theme::Theme;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

/// One window's stored session entry. `window` rides along so a
/// commit can take the public `ActivateRequested` path (which speaks
/// backend window ids), and `client` so window-menu and deminiaturize
/// verbs can name the client; both are re-validated by `wm-core` when
/// used, so a window that died mid-session costs a no-op, not a bug.
pub struct OverviewItem<B: Backend> {
    pub client: ClientId,
    pub window: B::WindowId,
    pub frame: Option<B::FrameId>,
    pub geometry: Rect,
    pub title: String,
    pub preview: Option<DecorationBuffer>,
    pub miniaturized: bool,
}

/// What a panel-local point lands on — the shell resolves clicks and
/// hover through this one function of the stored layout, so pixels
/// and hit-testing cannot disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewHit {
    Card(usize),
    Workspace(usize),
    CloseWorkspace(usize),
    Background,
}

#[derive(Clone, Copy)]
struct CardPress {
    client: ClientId,
    index: usize,
    start: Point,
    cell: Rect,
    workspace: usize,
    grab: DragHandle,
    dragging: bool,
    cancelled: bool,
}

/// A release can commit exactly the card armed by its matching press. A drag
/// never falls back to activation, even when dropped over another card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverviewRelease {
    Click(usize),
    Move { client: ClientId, source: usize, target: usize },
    Cancelled,
}

fn passed_threshold(start: Point, point: Point, threshold: i32) -> bool {
    let dx = i64::from(point.x) - i64::from(start.x);
    let dy = i64::from(point.y) - i64::from(start.y);
    // Unsigned saturating squares also handle adversarial/global coordinates.
    dx.unsigned_abs().saturating_pow(2).saturating_add(dy.unsigned_abs().saturating_pow(2))
        >= (threshold.max(1) as u64).pow(2)
}

fn drag_rect(cell: Rect, start: Point, point: Point, limit: Size) -> Rect {
    let scale = (limit.w as f64 / cell.size.w.max(1) as f64)
        .min(limit.h as f64 / cell.size.h.max(1) as f64).min(1.0);
    let offset_x = ((i64::from(start.x) - i64::from(cell.pos.x)) as f64 * scale).round() as i32;
    let offset_y = ((i64::from(start.y) - i64::from(cell.pos.y)) as f64 * scale).round() as i32;
    Rect::new(Point::new(point.x.saturating_sub(offset_x), point.y.saturating_sub(offset_y)),
        Size::new((cell.size.w as f64 * scale).round().max(1.0) as u32,
            (cell.size.h as f64 * scale).round().max(1.0) as u32))
}

pub struct OverviewPanel<B: Backend> {
    window: Option<B::ShellId>,
    /// The selection: highlight plate plus the awake card, on its own
    /// small surface over the panel — see the module doc's repaint
    /// discipline. Created beside the panel, moved per selection
    /// change, unmapped with it.
    selection: Option<B::ShellId>,
    /// The geometry the surfaces were created for; a differing primary
    /// rect on the next show recreates them.
    geometry: Rect,
    items: Vec<OverviewItem<B>>,
    selected: usize,
    layout: Option<OverviewLayout>,
    workspace: (usize, usize),
    visible: bool,
    /// Set at show time when the backend may still owe sharper
    /// previews than it answered with (see the module doc); cleared by
    /// the one catch-up fetch. `preview_generation` is the backend
    /// counter reading that catch-up waits to see move.
    awaiting_previews: bool,
    preview_generation: u64,
    live: bool,
    press: Option<CardPress>,
    drag: Option<OverviewDrag>,
    drag_threshold: i32,
    drag_limit: Size,
}

impl<B: Backend> Default for OverviewPanel<B> {
    fn default() -> Self {
        Self {
            window: None,
            selection: None,
            geometry: Rect::default(),
            items: Vec::new(),
            selected: 0,
            layout: None,
            workspace: (0, 1),
            visible: false,
            awaiting_previews: false,
            preview_generation: 0,
            live: false,
            press: None,
            drag: None,
            drag_threshold: 3,
            drag_limit: Size::new(168, 112),
        }
    }
}

impl<B: Backend> OverviewPanel<B> {
    /// Opens (or, while already open, re-populates) the panel over
    /// `primary` with a fresh entry set. `tile` is the Clip/dock tile
    /// edge, which sizes the workspace strip and derives the gutters.
    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
        primary: Rect,
        tile: u32,
        items: Vec<OverviewItem<B>>,
        workspace: (usize, usize),
        workspace_counts: &[usize],
        selected: usize,
    ) {
        self.invalidate_pointer(backend);
        if self.window.is_some() && self.geometry != primary {
            // The monitor arrangement moved under a kept surface; a
            // stale-sized buffer would letterbox or clip the panel.
            // Before the session state below is stored, because
            // discard clears a session's state along with its
            // surfaces.
            self.discard(backend);
        }
        self.selected = selected.min(items.len().saturating_sub(1));
        self.items = items;
        self.workspace = workspace;
        self.live = backend.supports_live_overview();
        self.drag_threshold = (tile as f64 * 3.0 / 56.0).ceil().max(2.0) as i32;
        self.drag_limit = Size::new(tile * 3, tile * 2);
        let layout = if self.live {
            let sizes: Vec<_> = self.items.iter().map(|item| item.geometry.size).collect();
            ov::live::layout(primary.size, tile, &sizes, workspace.1)
        } else {
            ov::layout(
                primary.size,
                tile,
                ov::header_height(theme),
                self.items.len(),
                workspace.1,
            )
        };
        // The card size is the preview resolution worth having, and
        // the backend must hear it before its next capture pass; the
        // catch-up bookkeeping is armed here so the fetch fires
        // exactly once per entry-set, when the counter moves.
        backend.set_preview_edge(if self.live {
            None
        } else {
            ov::capture_edge(&layout)
        });
        self.layout = Some(layout);
        self.awaiting_previews = !self.live && !self.items.is_empty();
        self.preview_generation = backend.preview_generation();

        if self.window.is_none() {
            match backend.create_shell_surface(primary, wm_theme::switcher::panel_background(theme), true) {
                Some(window) => {
                    self.window = Some(window);
                    self.geometry = primary;
                }
                None => {
                    tracing::warn!("failed to create the overview surface");
                    return;
                }
            }
        }
        if let Some(window) = self.window {
            if self.live {
                let layout = self.layout.as_ref().unwrap();
                let label_h = (tile / 2).max(16);
                let windows = self
                    .items
                    .iter()
                    .zip(&layout.cells)
                    .map(|(item, cell)| wm_core::OverviewWindow {
                        window: item.window,
                        frame: item.frame,
                        source: item.geometry,
                        destination: *cell,
                        label: ov::live::label(
                            theme,
                            font_system,
                            swash_cache,
                            &item.title,
                            (tile * 6).min(primary.size.w),
                            label_h,
                        ),
                    })
                    .collect();
                let spaces = layout
                    .strip
                    .iter()
                    .enumerate()
                    .map(|(i, rect)| wm_core::OverviewWorkspace {
                            rect: *rect,
                            label: ov::live::label(
                                theme,
                                font_system,
                                swash_cache,
                                &format!("Desktop {} · {}", i + 1, workspace_counts.get(i).copied().unwrap_or(0)),
                                rect.size.w,
                                label_h,
                            ),
                            drop_label: ov::live::label(theme, font_system, swash_cache,
                                &format!("Move to Desktop {}", i + 1), rect.size.w, label_h),
                            close: layout.workspace_close_rect(i)
                                .map(|rect| (rect, ov::workspace_close_glyph(rect.size.w))),
                    })
                    .collect();
                backend.show_live_overview(
                    window,
                    wm_core::OverviewScene {
                        geometry: primary,
                        windows,
                        spaces,
                        workspace: workspace.0,
                        selected: self.selected,
                        gap: layout.pad,
                    },
                );
            }
            if !self.visible {
                backend.map_shell_surface(window);
                self.visible = true;
            }
            backend.raise_shell_surface(window);
        }
        if self.live {
            return;
        }
        self.repaint(backend, theme, font_system, swash_cache);
        // Selection after the panel, so its surface ends up stacked
        // over it — the order these two rise in *is* the z-order
        // contract (menus opened later rise later still, and stay
        // above both).
        self.place_selection(backend, theme, font_system, swash_cache);
    }

    /// Re-rasterizes the whole panel from the stored state. The one
    /// monitor-sized verb here — it runs when the entry set changes
    /// (entry, desk switch, a window closing underneath, sharper
    /// previews landing), and deliberately not on selection moves,
    /// which belong to the small surface `place_selection` manages.
    fn repaint(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
    ) {
        let (Some(window), Some(layout)) = (self.window, self.layout.as_ref()) else {
            return;
        };
        let started = std::time::Instant::now();
        let entries: Vec<OverviewEntry> = self
            .items
            .iter()
            .map(|item| OverviewEntry {
                title: &item.title,
                preview: item.preview.as_ref(),
                miniaturized: item.miniaturized,
            })
            .collect();
        let buffer = ov::render_overview(theme, font_system, swash_cache, &entries, self.workspace, layout);
        if buffer.width > 0 {
            backend.paint_shell_surface(window, &buffer);
        }
        tracing::debug!(elapsed_us = started.elapsed().as_micros() as u64, "overview panel repaint");
    }

    /// Puts the selection surface under the selected card: repaint the
    /// card-sized buffer (the title under the highlight is the card's
    /// own, so the pixels change with the index) and configure the
    /// surface to the plate rect. No panel work happens here — that is
    /// the whole performance story of this panel.
    fn place_selection(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
    ) {
        if self.live {
            backend.select_live_overview(self.selected);
            return;
        }
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let (Some(item), Some(cell)) = (self.items.get(self.selected), layout.cells.get(self.selected)) else {
            // No cards on this desk: nothing to select, nothing shown.
            if let Some(selection) = self.selection {
                backend.unmap_shell_surface(selection);
                backend.release_shell_buffer(selection);
            }
            return;
        };
        let started = std::time::Instant::now();
        let cell = self.drag.map_or(*cell, |drag| drag.destination);
        let plate = ov::plate_rect(cell, layout.pad);
        // The layout speaks panel-local coordinates; surfaces live in
        // the global space the panel's own rect is in.
        let global = Rect {
            pos: Point::new(plate.pos.x + self.geometry.pos.x, plate.pos.y + self.geometry.pos.y),
            size: plate.size,
        };
        if self.selection.is_none() {
            self.selection = backend.create_shell_surface(global, wm_theme::switcher::panel_background(theme), true);
            if self.selection.is_none() {
                // Degraded but honest: the panel still works, the
                // selection is just invisible. Arrow keys and commit
                // stay correct because they read `self.selected`, not
                // pixels.
                tracing::warn!("failed to create the overview selection surface");
                return;
            }
        }
        let Some(selection) = self.selection else { return };
        let entry = OverviewEntry {
            title: &item.title,
            preview: item.preview.as_ref(),
            miniaturized: item.miniaturized,
        };
        let buffer = ov::render_selection(theme, font_system, swash_cache, &entry, cell.size, layout.pad);
        backend.configure_shell_surface(selection, global);
        if buffer.width > 0 {
            backend.paint_shell_surface(selection, &buffer);
        }
        if self.visible {
            backend.map_shell_surface(selection);
        }
        backend.raise_shell_surface(selection);
        tracing::debug!(elapsed_us = started.elapsed().as_micros() as u64, "overview selection move");
    }

    /// Moves the selection to `index`, restaging the selection surface
    /// only on change. Returns whether it changed — the hover path
    /// uses that to stay quiet while the pointer wanders inside one
    /// card.
    pub fn select(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
        index: usize,
    ) -> bool {
        if self.items.is_empty() {
            return false;
        }
        let index = index.min(self.items.len() - 1);
        if index == self.selected {
            return false;
        }
        self.selected = index;
        self.place_selection(backend, theme, font_system, swash_cache);
        true
    }

    /// Arrow-key movement, `(dx, dy)` in single steps, clamped by the
    /// pure grid math this panel was laid out with.
    pub fn move_selection(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
        dx: i32,
        dy: i32,
    ) {
        let Some(layout) = self.layout.as_ref() else {
            return;
        };
        let next = ov::move_selection(self.selected, self.items.len(), layout.cols, dx, dy);
        self.select(backend, theme, font_system, swash_cache, next);
    }

    /// What a panel-local point is over.
    pub fn hit(&self, local: Point) -> OverviewHit {
        let Some(layout) = self.layout.as_ref() else {
            return OverviewHit::Background;
        };
        if let Some(index) = layout.workspace_close_at(local) {
            return OverviewHit::CloseWorkspace(index);
        }
        if let Some(index) = layout.cell_at(local) {
            return OverviewHit::Card(index);
        }
        if let Some(index) = layout.workspace_at(local) {
            return OverviewHit::Workspace(index);
        }
        OverviewHit::Background
    }

    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Whether `surface` is part of the open session — the panel or
    /// the selection surface stacked over it. Input routing treats the
    /// two as one panel; [`OverviewPanel::panel_point`] maps either
    /// surface's local coordinates into the panel's.
    pub fn owns(&self, surface: B::ShellId) -> bool {
        self.visible && (self.window == Some(surface) || self.selection == Some(surface))
    }

    /// Translates a point local to one of the owned surfaces into
    /// panel-local coordinates, where the layout's hit-testing lives.
    /// A click lands on the selection surface precisely when the
    /// pointer is over the selected card's plate, and it must resolve
    /// to the same card the panel would have answered.
    pub fn panel_point(&self, surface: B::ShellId, local: Point) -> Point {
        if self.selection == Some(surface) {
            if let Some(layout) = self.layout.as_ref() {
                if let Some(cell) = layout.cells.get(self.selected) {
                    let plate = ov::plate_rect(self.drag.map_or(*cell, |d| d.destination), layout.pad);
                    return Point::new(local.x + plate.pos.x, local.y + plate.pos.y);
                }
            }
        }
        local
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn workspace(&self) -> (usize, usize) {
        self.workspace
    }

    pub fn pointer_pending(&self) -> bool {
        self.press.is_some()
    }

    /// Grab on press, not on crossing the threshold: a quick throw can leave
    /// the output before its next motion, but its release still belongs here.
    pub fn pointer_press(&mut self, backend: &mut B, index: usize, local: Point) {
        self.end_pointer(backend);
        let (Some(item), Some(cell)) = (self.items.get(index), self.layout.as_ref().and_then(|l| l.cells.get(index))) else { return };
        self.press = Some(CardPress {
            client: item.client, index, start: local, cell: *cell,
            workspace: self.workspace.0, grab: backend.grab_pointer_for_drag(),
            dragging: false, cancelled: false,
        });
    }

    /// Invalidation consumes the eventual release instead of letting it click
    /// a newly laid-out card or a desktop close control. The grab stays owned
    /// until release (or teardown), including after Escape while still held.
    fn invalidate_pointer(&mut self, backend: &mut B) {
        if let Some(press) = &mut self.press {
            press.cancelled = true;
        }
        self.drag = None;
        backend.drag_live_overview(None);
    }

    fn end_pointer(&mut self, backend: &mut B) {
        self.drag = None;
        backend.drag_live_overview(None);
        if let Some(press) = self.press.take() {
            backend.ungrab_pointer(press.grab);
        }
    }

    pub fn cancel_pointer(&mut self, backend: &mut B, theme: &Theme,
        fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache) -> bool {
        if !self.press.is_some_and(|p| !p.cancelled) { return false; }
        self.invalidate_pointer(backend);
        self.place_selection(backend, theme, fonts, cache);
        true
    }

    pub fn pointer_motion(&mut self, backend: &mut B, theme: &Theme,
        fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache, root: Point) -> bool {
        let local = Point::new(root.x.saturating_sub(self.geometry.pos.x), root.y.saturating_sub(self.geometry.pos.y));
        self.update_pointer(backend, theme, fonts, cache, local)
    }

    fn update_pointer(&mut self, backend: &mut B, theme: &Theme,
        fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache, local: Point) -> bool {
        let Some(press) = &mut self.press else { return false };
        if press.cancelled { return true; }
        press.dragging |= passed_threshold(press.start, local, self.drag_threshold);
        if !press.dragging { return true; }
        let workspace = self.layout.as_ref().and_then(|l| l.workspace_at(local))
            .filter(|target| *target != press.workspace);
        let drag = OverviewDrag { index: press.index,
            destination: drag_rect(press.cell, press.start, local, self.drag_limit), workspace };
        let first = self.drag.is_none();
        if self.drag == Some(drag) { return true; }
        self.drag = Some(drag);
        if self.live {
            backend.drag_live_overview(Some(drag));
        } else if first {
            // Legacy X11 retains the existing small raster selection as its
            // drag image. Paint once on pickup; subsequent motions only move it.
            self.place_selection(backend, theme, fonts, cache);
        } else if let (Some(selection), Some(layout)) = (self.selection, &self.layout) {
            let plate = ov::plate_rect(drag.destination, layout.pad);
            backend.configure_shell_surface(selection, Rect::new(
                Point::new(plate.pos.x + self.geometry.pos.x, plate.pos.y + self.geometry.pos.y), plate.size));
        }
        true
    }

    pub fn pointer_release(&mut self, backend: &mut B, theme: &Theme,
        fonts: &mut cosmic_text::FontSystem, cache: &mut cosmic_text::SwashCache,
        local: Point) -> Option<OverviewRelease> {
        // A coalesced press/motion/release burst may not have delivered an
        // intermediate motion to the shell. The release's coordinates count.
        self.update_pointer(backend, theme, fonts, cache, local);
        let press = self.press?;
        let result = if press.cancelled { OverviewRelease::Cancelled }
        else if press.dragging {
            self.drag.and_then(|d| d.workspace).map_or(OverviewRelease::Cancelled, |target|
                OverviewRelease::Move { client: press.client, source: press.workspace, target })
        } else if self.hit(local) == OverviewHit::Card(press.index) {
            OverviewRelease::Click(press.index)
        } else { OverviewRelease::Cancelled };
        self.end_pointer(backend);
        self.place_selection(backend, theme, fonts, cache);
        Some(result)
    }

    pub fn item(&self, index: usize) -> Option<&OverviewItem<B>> {
        self.items.get(index)
    }

    /// The clients of the current entry set, in card order — what the
    /// catch-up preview fetch asks the window manager about.
    pub fn clients(&self) -> Vec<ClientId> {
        self.items.iter().map(|item| item.client).collect()
    }

    /// Whether the one-shot preview catch-up should fire: a session is
    /// open, entry noted that sharper previews may still be owed, and
    /// the backend's counter has since moved (a backend that answers
    /// captures synchronously never moves it, so this never fires
    /// there). See the module doc's preview-resolution story.
    pub fn wants_fresh_previews(&self, generation: u64) -> bool {
        self.visible && self.awaiting_previews && generation != self.preview_generation
    }

    /// Installs the previews the catch-up fetched — item order, i.e.
    /// [`OverviewPanel::clients`] order — and repaints panel and
    /// selection once. A `None` keeps the preview already held: a
    /// capture that failed must not blank a card that had something.
    pub fn update_previews(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        swash_cache: &mut cosmic_text::SwashCache,
        previews: Vec<Option<DecorationBuffer>>,
        generation: u64,
    ) {
        self.awaiting_previews = false;
        self.preview_generation = generation;
        for (item, preview) in self.items.iter_mut().zip(previews) {
            if preview.is_some() {
                item.preview = preview;
            }
        }
        self.repaint(backend, theme, font_system, swash_cache);
        self.place_selection(backend, theme, font_system, swash_cache);
    }

    /// Closes the session: unmap, keep the surfaces (see the module
    /// doc), drop the captured previews — they are stale the moment
    /// the desktop is interactive again, and holding N window-sized
    /// buffers between sessions buys nothing. The preview-edge hint is
    /// withdrawn with the session: the backend's snapshots go back to
    /// icon-sized on their own schedule.
    pub fn hide(&mut self, backend: &mut B) {
        self.end_pointer(backend);
        if self.live {
            backend.hide_live_overview();
        }
        if let Some(window) = self.window {
            backend.unmap_shell_surface(window);
            backend.release_shell_buffer(window);
        }
        if let Some(selection) = self.selection {
            backend.unmap_shell_surface(selection);
            backend.release_shell_buffer(selection);
        }
        backend.set_preview_edge(None);
        self.visible = false;
        self.items.clear();
        self.layout = None;
        self.awaiting_previews = false;
    }

    /// Destroys the surfaces outright — for a restyle, rescale or
    /// monitor change, after which their sizes and pixels are both
    /// wrong. Also lets go of the modal keyboard grab when a session
    /// was live: the grab was taken for this panel, and a reload
    /// marker (touched from a terminal on another VT, say) can land
    /// mid-session — leaving the keyboard grabbed with no panel to
    /// serve would wedge every key on the desk. Ungrabbing when not
    /// grabbed is a no-op on both backends.
    pub fn discard(&mut self, backend: &mut B) {
        self.end_pointer(backend);
        if self.live {
            backend.hide_live_overview();
        }
        if self.visible {
            backend.ungrab_keyboard();
        }
        if let Some(window) = self.window.take() {
            backend.destroy_shell_surface(window);
        }
        if let Some(selection) = self.selection.take() {
            backend.destroy_shell_surface(selection);
        }
        backend.set_preview_edge(None);
        self.visible = false;
        self.items.clear();
        self.layout = None;
        self.geometry = Rect::default();
        self.awaiting_previews = false;
    }

    /// The panel's current size, for tests and diagnostics.
    pub fn size(&self) -> Size {
        self.geometry.size
    }
}

#[cfg(test)]
mod drag_tests {
    use super::*;
    #[test]
    fn threshold_is_radial_scale_aware_and_overflow_safe() {
        for scale in [1, 2] {
            assert!(!passed_threshold(Point::new(0, 0), Point::new(scale, scale), 3 * scale));
            assert!(passed_threshold(Point::new(0, 0), Point::new(3 * scale, 0), 3 * scale));
        }
        assert!(passed_threshold(Point::new(i32::MIN, i32::MIN), Point::new(i32::MAX, i32::MAX), 3));
    }
    #[test]
    fn drag_image_keeps_the_grab_anchor_and_never_upscales() {
        let cell = Rect::new(Point::new(100, 200), Size::new(400, 200));
        let rect = drag_rect(cell, Point::new(200, 250), Point::new(800, 300), Size::new(200, 100));
        assert_eq!(rect, Rect::new(Point::new(750, 275), Size::new(200, 100)));
        let small = Rect::new(Point::new(100, 200), Size::new(40, 20));
        assert_eq!(drag_rect(small, small.pos, Point::new(500, 300), Size::new(200, 100)).size, small.size);
    }
}
