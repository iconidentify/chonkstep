//! The Overview's surface and session state: the shell half of the
//! modal Exposé-style panel `wm_theme::overview` rasterizes. This type
//! owns the full-screen shell surface, the captured entries, the
//! selection, and the layout used for hit-testing; the `Shell`
//! orchestrator owns the modality around it (when it opens, the
//! keyboard grab, what a click or key means), and `Desktop` owns the
//! font state it renders with — so the methods here take fonts as
//! parameters and `Desktop` wraps them, exactly the switcher's shape.
//!
//! # Repaint discipline
//!
//! The panel surface covers the whole primary monitor — at the
//! reference desk (3840x2160, scale 2) that is a ~33MB buffer — so it
//! is rendered on entry and on *entry-set change* (entries replaced,
//! fresher previews arriving), never per frame and never per
//! selection move. The selection lives on a second, card-sized
//! surface stacked over the panel: moving it is a configure (a
//! position change the compositor applies for free) plus a repaint of
//! that one card. The first cut painted the selection into the panel
//! itself, which meant every card the pointer crossed re-rasterized
//! and re-uploaded the monitor — measured at ~2.1s a crossing on a
//! debug build, which is what "the Overview drags the mouse" was.
//! A hover that stays inside the already-selected card still repaints
//! nothing at all.
//!
//! # Preview resolution
//!
//! Cards are card-sized, so entry hints the card width to the backend
//! (`Backend::set_preview_edge`) before fetching previews. A backend
//! that captures synchronously (X11) is sharp immediately; the
//! compositor serves its throttled snapshots — icon-sized — and
//! honors the hint on the next rendered frame, after which its
//! `preview_generation` moves and `wants_fresh_previews` tells the
//! shell to fetch again: one extra panel paint, a frame or two after
//! entry, in exchange for text in the cards being text.
//!
//! # Surface lifecycle
//!
//! The windows are unmapped between sessions, not destroyed, and only
//! recreated when the monitor geometry changes — the switcher panel's
//! rule, adopted for its reason (destroy/recreate churn wedged a
//! session compositor once; see `SwitcherPanel`'s doc) plus a new one:
//! the surface identity is cheap to preserve. Its monitor-sized pixels are
//! released while hidden and rebuilt before the surface is mapped again.

use wm_core::{Backend, ClientId};
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
    Background,
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
        selected: usize,
    ) {
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
        let layout = ov::layout(primary.size, tile, ov::header_height(theme), self.items.len(), workspace.1);
        // The card size is the preview resolution worth having, and
        // the backend must hear it before its next capture pass; the
        // catch-up bookkeeping is armed here so the fetch fires
        // exactly once per entry-set, when the counter moves.
        backend.set_preview_edge(ov::capture_edge(&layout));
        self.layout = Some(layout);
        self.awaiting_previews = !self.items.is_empty();
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
            if !self.visible {
                backend.map_shell_surface(window);
                self.visible = true;
            }
            backend.raise_shell_surface(window);
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
        let plate = ov::plate_rect(*cell, layout.pad);
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
                    let plate = ov::plate_rect(*cell, layout.pad);
                    return Point::new(local.x + plate.pos.x, local.y + plate.pos.y);
                }
            }
        }
        local
    }

    pub fn selected(&self) -> usize {
        self.selected
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
