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
//! The surface covers the whole primary monitor — at the reference
//! desk (3840x2160, scale 2) that is a ~33MB buffer — so it is
//! rendered on entry and on *state change* (selection moved, entries
//! replaced), never per frame: the same rule `redraw_dock` follows,
//! applied to a much bigger pixmap. A hover that stays inside the
//! already-selected card repaints nothing.
//!
//! # Surface lifecycle
//!
//! The window is unmapped between sessions, not destroyed, and only
//! recreated when the monitor geometry changes — the switcher panel's
//! rule, adopted for its reason (destroy/recreate churn wedged a
//! session compositor once; see `SwitcherPanel`'s doc) plus a new one:
//! this surface's backing buffer is large, and reallocating it on
//! every entry is the most expensive way to obtain the buffer we just
//! had.

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
    /// The geometry the surface was created for; a differing primary
    /// rect on the next show recreates it.
    geometry: Rect,
    items: Vec<OverviewItem<B>>,
    selected: usize,
    layout: Option<OverviewLayout>,
    workspace: (usize, usize),
    visible: bool,
}

impl<B: Backend> Default for OverviewPanel<B> {
    fn default() -> Self {
        Self {
            window: None,
            geometry: Rect::default(),
            items: Vec::new(),
            selected: 0,
            layout: None,
            workspace: (0, 1),
            visible: false,
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
        self.selected = selected.min(items.len().saturating_sub(1));
        self.items = items;
        self.workspace = workspace;
        self.layout = Some(ov::layout(primary.size, tile, ov::header_height(theme), self.items.len(), workspace.1));

        if self.window.is_some() && self.geometry != primary {
            // The monitor arrangement moved under a kept surface; a
            // stale-sized buffer would letterbox or clip the panel.
            self.discard(backend);
        }
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
    }

    /// Re-rasterizes the whole panel from the stored state. The one
    /// expensive verb here — every mutator below calls it only when
    /// something visible actually changed.
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
        let entries: Vec<OverviewEntry> = self
            .items
            .iter()
            .map(|item| OverviewEntry {
                title: &item.title,
                preview: item.preview.as_ref(),
                miniaturized: item.miniaturized,
            })
            .collect();
        let buffer = ov::render_overview(theme, font_system, swash_cache, &entries, self.selected, self.workspace, layout);
        if buffer.width > 0 {
            backend.paint_shell_surface(window, &buffer);
        }
    }

    /// Moves the selection to `index`, repainting only on change.
    /// Returns whether it changed — the hover path uses that to stay
    /// quiet while the pointer wanders inside one card.
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
        self.repaint(backend, theme, font_system, swash_cache);
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

    pub fn owns(&self, surface: B::ShellId) -> bool {
        self.visible && self.window == Some(surface)
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn item(&self, index: usize) -> Option<&OverviewItem<B>> {
        self.items.get(index)
    }

    /// Closes the session: unmap, keep the surface (see the module
    /// doc), drop the captured previews — they are stale the moment
    /// the desktop is interactive again, and holding N window-sized
    /// buffers between sessions buys nothing.
    pub fn hide(&mut self, backend: &mut B) {
        if let Some(window) = self.window {
            backend.unmap_shell_surface(window);
        }
        self.visible = false;
        self.items.clear();
        self.layout = None;
    }

    /// Destroys the surface outright — for a restyle, rescale or
    /// monitor change, after which its size and pixels are both wrong.
    /// Also lets go of the modal keyboard grab when a session was
    /// live: the grab was taken for this panel, and a reload marker
    /// (touched from a terminal on another VT, say) can land mid-
    /// session — leaving the keyboard grabbed with no panel to serve
    /// would wedge every key on the desk. Ungrabbing when not grabbed
    /// is a no-op on both backends.
    pub fn discard(&mut self, backend: &mut B) {
        if self.visible {
            backend.ungrab_keyboard();
        }
        if let Some(window) = self.window.take() {
            backend.destroy_shell_surface(window);
        }
        self.visible = false;
        self.items.clear();
        self.layout = None;
        self.geometry = Rect::default();
    }

    /// The panel's current size, for tests and diagnostics.
    pub fn size(&self) -> Size {
        self.geometry.size
    }
}
