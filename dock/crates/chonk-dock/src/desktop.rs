//! The original dock presentation and interaction code, extracted from
//! chonk-shell before 5d65649. Surfaces now belong to a client process;
//! no window-management backend or compositor state enters this module.
use crate::dockapp::panel::{self as instrument, InstrumentPanel};
use crate::dockapp::tile::{
    clamp_panel_grant, reserved_filter, RemoteTile, ServiceContext, StopReason, TileState,
};
use crate::dockapp::{self, DockHost, Farewell};
use crate::surface::{Backend, DragHandle};
use crate::widgets::*;
use chonk_dock_proto::wire::{InputEvent, InputKind, PanelCloseReason};
use std::cell::RefCell;
#[cfg(test)]
use std::path::{Path, PathBuf};
use std::rc::Rc;
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wm_theme::{paint, panel, tile, workspace, Theme};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};
pub const DESKTOP_BG: (u8, u8, u8) = (128, 129, 159);
pub(crate) mod dock_order {
    use std::path::{Path, PathBuf};

    /// `$XDG_STATE_HOME/chonkstep/dock-items`, or the `~/.local/state`
    /// fallback — the same resolution as `launchdock`'s `dock` file,
    /// `theme_select.rs`'s and `wallpaper.rs`'s, which all live in this
    /// same directory. The name is `dock-items` rather than `dock`
    /// because `dock` is already taken by the launcher strip's pins,
    /// and two files a user may edit by hand should not be one
    /// character apart in meaning.
    pub(crate) fn state_path() -> Option<PathBuf> {
        crate::startup::state_file("dock-items")
    }

    /// The remembered order, or an empty list if there is no file yet
    /// (a fresh session, which then gets the built-in default order).
    ///
    /// Blank lines and `#` comments are skipped, because this is a file
    /// people are invited to edit and a file people edit acquires
    /// comments.
    pub(crate) fn load(path: &Path) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    /// Writes one id per line.
    pub(crate) fn save(path: &Path, ids: &[String]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for id in ids {
            text.push_str(id);
            text.push('\n');
        }
        std::fs::write(path, text)
    }

    /// Sorts `items` into the remembered order.
    ///
    /// Two rules, and both matter:
    ///
    /// * A remembered id with no live item is skipped here and
    ///   *preserved* by [`merge`] on the next write. See the module
    ///   docs.
    /// * A live item nobody remembers keeps its position relative to
    ///   the other unremembered items and lands after everything that
    ///   was remembered. That is what happens when an upgrade adds a
    ///   seventh instrument or the user drops in a new dockapp: it
    ///   appears at the bottom of the column, which is predictable,
    ///   rather than in the middle of an arrangement they built.
    ///
    /// Generic over the key so the whole rule is testable against
    /// plain strings, with no dock, no backend and no widgets.
    pub(crate) fn arrange<T>(
        items: Vec<T>,
        order: &[String],
        id_of: impl Fn(&T) -> String,
    ) -> Vec<T> {
        if order.is_empty() {
            return items;
        }
        let mut keyed: Vec<(String, Option<T>)> = items
            .into_iter()
            .map(|item| (id_of(&item), Some(item)))
            .collect();
        let mut arranged = Vec::with_capacity(keyed.len());
        for wanted in order {
            if let Some(slot) = keyed
                .iter_mut()
                .find(|(id, item)| id == wanted && item.is_some())
            {
                arranged.push(slot.1.take().expect("just checked it is Some"));
            }
        }
        arranged.extend(keyed.into_iter().filter_map(|(_, item)| item));
        arranged
    }

    /// The line-for-line contents to write after a reorder: the live
    /// column, plus every remembered id that did not resolve this
    /// session, put back where it was.
    ///
    /// "Where it was" means *after the same neighbour it used to
    /// follow*. Walking `remembered` and re-inserting each unresolved
    /// id after its nearest still-live predecessor keeps a dockapp that
    /// sat between the clock and the power tile between the clock and
    /// the power tile, through however many sessions it takes for its
    /// registry file to come back. Appending them at the end would be
    /// simpler and would quietly relocate every one of them to the
    /// bottom of the dock — which is the same forgetting this exists to
    /// prevent, one step slower.
    pub(crate) fn merge(live: &[String], remembered: &[String]) -> Vec<String> {
        let mut out: Vec<String> = live.to_vec();
        let mut anchor: Option<String> = None;
        for id in remembered {
            if live.contains(id) {
                anchor = Some(id.clone());
                continue;
            }
            if out.contains(id) {
                continue;
            }
            let at = match &anchor {
                // Position after the neighbour it used to follow. The
                // anchor is live by construction, so the `position` is
                // always found.
                Some(previous) => out
                    .iter()
                    .position(|live| live == previous)
                    .map_or(0, |index| index + 1),
                // It was the very first line, and nothing before it
                // survives: the top of the column is where it goes.
                None => 0,
            };
            out.insert(at, id.clone());
            anchor = Some(id.clone());
        }
        out
    }
}

fn stacked_dock_height(tile: u32, screen_height: u32, items: &[SupervisedWidget]) -> u32 {
    items
        .iter()
        // `SupervisedWidget::tile_height` already floors a widget's own
        // answer at one tile, and already answers exactly one for an
        // evicted widget — so an evicted multi-tile instrument shrinks
        // the dock rather than leaving a hole where its extra tiles
        // used to be.
        .fold(tile, |height, item| {
            height.saturating_add(tile.saturating_mul(item.tile_height()))
        })
        .min(screen_height.max(1))
}
fn dock_geometry(
    primary: Rect,
    reserved: EdgeReservation,
    dock_width: u32,
    dock_height: u32,
) -> Rect {
    Rect {
        pos: Point::new(
            primary.pos.x + primary.size.w.saturating_sub(dock_width + reserved.right) as i32,
            primary.pos.y + reserved.top.min(primary.size.h) as i32,
        ),
        size: Size::new(dock_width, dock_height),
    }
}
fn clip_geometry(primary: Rect, tile: u32) -> Rect {
    Rect {
        pos: Point::new(
            primary.pos.x + primary.size.w.saturating_sub(tile) as i32,
            primary.pos.y + primary.size.h.saturating_sub(tile) as i32,
        ),
        size: Size::new(tile, tile),
    }
}
pub fn tile_px(scale: f32) -> u32 {
    ((56.0 * scale).round() as u32).max(16)
}
pub fn drag_threshold_px(scale: f32) -> i32 {
    ((4.0 * scale).round() as i32).max(2)
}
fn builtin_panel_event(event: &InputEvent) -> PanelEvent {
    let local = Point::new(event.x, event.y);
    match event.kind {
        InputKind::Press => PanelEvent::LeftPress { local },
        InputKind::Release => PanelEvent::LeftRelease { local },
        InputKind::Scroll => PanelEvent::Scroll {
            local,
            delta: event.delta,
        },
        InputKind::Motion => PanelEvent::Motion { local },
        InputKind::Enter => PanelEvent::Enter,
        InputKind::Leave => PanelEvent::Leave,
    }
}
fn builtin_items() -> Vec<DockItem> {
    vec![
        DockItem::builtin(
            "builtin:net",
            Box::new(NetTrafficWidget::new()) as Box<dyn DockWidget>,
        ),
        DockItem::builtin("builtin:sysload", Box::new(SysLoadWidget::new())),
        DockItem::builtin("builtin:sound", Box::new(SoundWidget::new())),
        DockItem::builtin("builtin:wifi", Box::new(WifiWidget::new())),
        DockItem::builtin("builtin:bluetooth", Box::new(BluetoothWidget::new())),
        DockItem::builtin("builtin:power", Box::new(PowerWidget::new())),
        DockItem::builtin("builtin:clock", Box::new(ClockWidget::new())),
    ]
}
fn scale_mark(src: &Pixmap, size: u32) -> Option<Pixmap> {
    if size == 0 || src.width() == size {
        return None;
    }
    let source = image::RgbaImage::from_raw(src.width(), src.height(), src.data().to_vec())?;
    let resized =
        image::imageops::resize(&source, size, size, image::imageops::FilterType::Lanczos3);
    let mut out = Pixmap::new(size, size)?;
    out.data_mut().copy_from_slice(resized.as_raw());
    Some(out)
}
fn pixmap_to_buffer(pixmap: Pixmap) -> DecorationBuffer {
    DecorationBuffer {
        width: pixmap.width(),
        height: pixmap.height(),
        pixels: pixmap.take(),
    }
}
pub(crate) fn blit_into(dest: &mut Pixmap, x: u32, y: u32, src: &DecorationBuffer) {
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
            let (r, g, b, a) = (
                src.pixels[sidx],
                src.pixels[sidx + 1],
                src.pixels[sidx + 2],
                src.pixels[sidx + 3],
            );
            if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a) {
                let pidx = (dy * dest_w + dx) as usize;
                dest.pixels_mut()[pidx] = px;
            }
        }
    }
}
struct ItemDrag {
    index: usize,
    grab: DragHandle,
}
struct BuiltinPanel {
    /// The owning item's persistence id (`builtin:*`) — the same key
    /// the surface's owner field carries, so click-away, toggle and
    /// teardown resolve through one comparison whatever kind of tile
    /// owns the panel.
    id: String,
    /// The granted content size: the widget's [`PanelSpec`] clamped by
    /// [`crate::dockapp::tile::clamp_panel_grant`], through the same
    /// arithmetic a remote `OpenPanel` is granted by, so a built-in
    /// cannot be granted a panel a dockapp would have been refused.
    ///
    /// [`PanelSpec`]: chonk_dock_widget::PanelSpec
    granted: (u32, u32),
    /// The persistent granted-size frame the widget renders into —
    /// kept across repaints so a widget may redraw only what changed.
    frame: PanelFrame,
    /// This panel was opened (or re-opened) since the last
    /// reconciliation pass — the arbitration token
    /// [`Desktop::sync_instrument_panel`] consumes to decide the
    /// desktop-wide winner. The twin of `PanelState::just_opened`.
    just_opened: bool,
    /// The frame is stale: the widget asked for a repaint
    /// ([`PanelReaction::Repaint`]) or has never rendered. Cleared by
    /// the reconciliation pass that re-renders and presents.
    dirty: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct EdgeReservation {
    pub top: u32,
    pub right: u32,
}
fn dock_mark() -> Pixmap {
    Pixmap::decode_png(include_bytes!("../assets/branding/chonkstep-logo-icon.png"))
        .expect("embedded mark")
}
pub struct Desktop<B: Backend> {
    dock_window: B::ShellId,
    primary: Rect,
    reserved: EdgeReservation,
    dock_width: u32,
    tile: u32,
    fonts: wm_theme::FontState,
    items: Vec<SupervisedWidget>,
    dockapps: DockHost,
    scale: f32,
    remembered_order: Vec<String>,
    samplers: SamplerRegistry,
    workspace: Rc<RefCell<WorkspaceShared>>,
    clip_window: B::ShellId,
    clip_drawn: (usize, usize),
    item_drag: Option<ItemDrag>,
    hovered_item: Option<String>,
    appearance: wm_theme::Appearance,
    instrument_panel: InstrumentPanel<B>,
    builtin_panel: Option<BuiltinPanel>,
    escape_key_grabbed: bool,
    panel_swallow_release: Option<String>,
    theme_id: String,
    logo: Pixmap,
    logo_scaled: Option<(u32, Pixmap)>,
}
impl<B: Backend> Desktop<B> {
    pub fn new(
        backend: &mut B,
        primary: Rect,
        scale: f32,
        theme: &Theme,
        fonts: wm_theme::FontState,
    ) -> Self {
        let tile = tile_px(scale);
        let dockapps = DockHost::new(&backend.display_name());
        let registered = if dockapps.is_listening() {
            dockapp::registry::scan()
        } else {
            Vec::new()
        };
        let now = std::time::Instant::now();
        let mut inherited = dockapps
            .handoff_path()
            .map(|path| dockapp::handoff::take(&path))
            .unwrap_or_default();
        let mut samplers = SamplerRegistry::new();
        samplers.set_active(false);
        let items = builtin_items()
            .into_iter()
            .chain(registered.into_iter().map(|entry| {
                let mut remote = RemoteTile::new(entry, tile, now);
                if let Some(token) = inherited.remove(remote.id()) {
                    remote.rejoin(token, now);
                }
                DockItem::Remote(Box::new(remote))
            }))
            .map(SupervisedWidget::new)
            .map(|mut item| {
                item.bind(&mut samplers);
                item
            })
            .collect();
        let remembered_order = dock_order::state_path()
            .map(|path| dock_order::load(&path))
            .unwrap_or_default();
        let items = dock_order::arrange(items, &remembered_order, |item| item.id().to_string());
        let reserved = EdgeReservation::default();
        let geom = dock_geometry(
            primary,
            reserved,
            tile,
            stacked_dock_height(tile, primary.size.h, &items),
        );
        let dock_window = backend
            .create_shell_surface(geom, DESKTOP_BG, true)
            .expect("dock surface");
        backend.set_role(dock_window, crate::surface::Role::Dock);
        backend.map_shell_surface(dock_window);
        let clip_window = backend
            .create_shell_surface(clip_geometry(primary, tile), DESKTOP_BG, true)
            .expect("clip surface");
        backend.set_role(clip_window, crate::surface::Role::Clip);
        backend.map_shell_surface(clip_window);
        let mut desktop = Self {
            dock_window,
            primary,
            reserved,
            dock_width: tile,
            tile,
            fonts,
            items,
            dockapps,
            scale,
            remembered_order,
            samplers,
            workspace: Rc::new(RefCell::new(WorkspaceShared {
                current: 0,
                count: 1,
                requested: None,
            })),
            clip_window,
            clip_drawn: (0, 1),
            item_drag: None,
            hovered_item: None,
            appearance: theme.appearance,
            instrument_panel: InstrumentPanel::default(),
            builtin_panel: None,
            escape_key_grabbed: false,
            panel_swallow_release: None,
            theme_id: theme.id.clone(),
            logo: dock_mark(),
            logo_scaled: None,
        };
        desktop.redraw_dock(backend, theme);
        desktop.repaint_clip(backend, theme);
        desktop
    }
    pub fn restyle(&mut self, backend: &mut B, primary: Rect, scale: f32, theme: &Theme) {
        self.dismiss_instrument_panel(backend, PanelCloseReason::Dismissed);
        self.instrument_panel.discard(backend);
        self.primary = primary;
        self.scale = scale;
        self.tile = tile_px(scale);
        self.dock_width = self.tile;
        self.appearance = theme.appearance;
        self.theme_id = theme.id.clone();
        for item in &mut self.items {
            if let Some(remote) = item.remote_mut() {
                remote.set_tile_px(self.tile);
            }
        }
        backend.configure_shell_surface(self.clip_window, clip_geometry(primary, self.tile));
        self.redraw_dock(backend, theme);
        self.repaint_clip(backend, theme);
    }
    pub fn dock_rect(&self) -> Rect {
        self.dock_geom()
    }
    pub fn dock_window(&self) -> B::ShellId {
        self.dock_window
    }

    pub fn primary_workarea(&self) -> Rect {
        let column = self.dock_width;
        let reserved_right = column.saturating_add(self.reserved.right);
        Rect {
            pos: self.primary.pos,
            size: Size::new(
                self.primary.size.w.saturating_sub(reserved_right).max(1),
                self.primary.size.h,
            ),
        }
    }

    fn dock_geom(&self) -> Rect {
        let below_bar = self.primary.size.h.saturating_sub(self.reserved.top);
        let dock_height = stacked_dock_height(self.tile, below_bar, &self.items);
        dock_geometry(self.primary, self.reserved, self.dock_width, dock_height)
    }

    pub fn tick_items(&mut self, backend: &mut B, theme: &Theme) {
        self.samplers.set_active(true);
        // Out-of-process tiles first, so a frame that arrived this pass
        // is folded by the same `update` sweep that folds a sampler
        // reading, and reaches the screen on the same repaint. Doing it
        // after would show every dockapp frame one servicing pass late for
        // no reason.
        self.service_dockapps(theme);
        // The panel rides the same cadence: reconcile what the tiles
        // negotiated this pass onto the one panel surface.
        self.sync_instrument_panel(backend, theme);
        self.samplers.refresh();
        let mut changed = false;
        {
            // Scoped so the borrow of `samplers` ends before the
            // repaint, which needs all of `self`.
            let samples = self.samplers.samples();
            for widget in &mut self.items {
                if widget.update(&samples) {
                    changed = true;
                }
            }
        }
        self.sync_builtin_sampling();
        if changed {
            self.redraw_dock(backend, theme);
        }
    }

    fn sync_builtin_sampling(&mut self) {
        let owner = self.builtin_panel.as_ref().map(|panel| panel.id.as_str());
        for item in &mut self.items {
            item.sync_sampling(&mut self.samplers, owner == Some(item.id()));
        }
    }

    pub fn set_workspace_display(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        current: usize,
        count: usize,
    ) {
        {
            let mut shared = self.workspace.borrow_mut();
            shared.current = current;
            shared.count = count;
        }
        if self.clip_drawn != (current, count) {
            self.clip_drawn = (current, count);
            self.repaint_clip(backend, theme);
        }
    }

    fn repaint_clip(&mut self, backend: &mut B, theme: &Theme) {
        // A hidden Clip is not drawn, for `redraw_dock`'s reason and
        // with its guarantee: `clip_drawn` is still updated by
        // `set_workspace_display` while the tile is away, so the switch
        // that happened out of sight is already recorded, and
        // `set_dock_visibility` repaints on the way up before mapping.
        // Skipping only the composite is what keeps a hidden Clip from
        // rendering a tile per workspace change that nobody can see.
        let (current, count) = self.clip_drawn;
        let current = if current == usize::MAX { 0 } else { current };
        let buffer = workspace::render_clip_tile(
            theme,
            &mut self.fonts.system(),
            &mut self.fonts.swash(),
            self.tile,
            current,
            count.max(1),
        );
        backend.paint_shell_surface(self.clip_window, &buffer);
    }

    pub fn clip_window(&self) -> B::ShellId {
        self.clip_window
    }

    pub fn click_clip(&mut self, local: Point) {
        let mut shared = self.workspace.borrow_mut();
        match workspace::clip_hit(self.tile, local.x, local.y) {
            workspace::ClipZone::Forward => shared.requested = Some(shared.current + 1),
            workspace::ClipZone::Rewind => shared.requested = shared.current.checked_sub(1),
            workspace::ClipZone::Body => {}
        }
    }

    pub fn take_workspace_request(&mut self) -> Option<usize> {
        self.workspace.borrow_mut().requested.take()
    }

    fn items_top(&self) -> i32 {
        self.tile as i32
    }

    fn item_slots(&self) -> impl Iterator<Item = (usize, Rect)> + '_ {
        // Hit-testing stops at the first match without allocating a
        // vector for every pointer report. Painting collects only the
        // visible prefix before mutably borrowing the widget renderers.
        self.items
            .iter()
            .enumerate()
            .scan(self.items_top(), |y, (index, widget)| {
                let h = self.tile * widget.tile_height();
                let rect = Rect {
                    pos: Point::new(0, *y),
                    size: Size::new(self.tile, h),
                };
                *y += h as i32;
                Some((index, rect))
            })
    }

    fn item_index_at(&self, local: Point) -> Option<usize> {
        self.item_slots()
            .find(|(_, rect)| rect.contains(local))
            .map(|(index, _)| index)
    }

    pub fn begin_item_drag(&mut self, backend: &mut B, theme: &Theme, local: Point) -> bool {
        let Some(index) = self.item_index_at(local) else {
            return false;
        };
        // Same reasoning as `begin_icon_drag`: without a grab, a fast
        // drag could outrun the dock's own (narrow) window bounds and
        // stop reporting motion against it.
        let grab = backend.grab_pointer_for_drag();
        self.item_drag = Some(ItemDrag { index, grab });
        self.redraw_dock(backend, theme);
        true
    }

    pub fn drag_item_motion(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        let Some(dragged) = self.item_drag.as_ref().map(|d| d.index) else {
            return;
        };
        let Some(target) = self.item_index_at(Point::new(0, root.y - self.dock_geom().pos.y))
        else {
            return;
        };
        if target == dragged {
            return;
        }
        self.items.swap(dragged, target);
        if let Some(drag) = &mut self.item_drag {
            drag.index = target;
        }
        self.redraw_dock(backend, theme);
    }

    pub fn end_item_drag(&mut self, backend: &mut B, theme: &Theme) -> bool {
        let Some(drag) = self.item_drag.take() else {
            return false;
        };
        backend.ungrab_pointer(drag.grab);
        self.persist_order();
        self.redraw_dock(backend, theme);
        true
    }

    fn persist_order(&mut self) {
        let Some(path) = dock_order::state_path() else {
            return;
        };
        let live: Vec<String> = self
            .items
            .iter()
            .map(|item| item.id().to_string())
            .collect();
        let merged = dock_order::merge(&live, &self.remembered_order);
        if let Err(error) = dock_order::save(&path, &merged) {
            tracing::warn!(?error, path = %path.display(), "failed to remember the dock's order");
            return;
        }
        self.remembered_order = merged;
    }

    pub fn dock_input(&mut self, backend: &mut B, theme: &Theme, input: DockInput) -> bool {
        let Some(local) = input.local() else {
            return false;
        };
        let Some((index, rect)) = self.item_slots().find(|(_, rect)| rect.contains(local)) else {
            return false;
        };
        // The panel's toggle and click-away, resolved before delivery.
        // A press on the tile whose panel is open is the toggle: the
        // panel closes and the press is *consumed* — forwarding it
        // would invite the dockapp to reopen what the user just closed
        // — and its release is swallowed with it, so the client never
        // sees half a click. A press on any other tile is an ordinary
        // click-away that then routes normally.
        if let DockInput::Press { .. } = input {
            if self.instrument_panel.visible() {
                let owner_clicked = self.instrument_panel.owner() == Some(self.items[index].id());
                self.dismiss_instrument_panel(backend, PanelCloseReason::Dismissed);
                if owner_clicked {
                    self.panel_swallow_release = Some(self.items[index].id().to_string());
                    return true;
                }
            }
        }
        if let DockInput::Release { .. } = input {
            if self
                .panel_swallow_release
                .take()
                .is_some_and(|id| id == self.items[index].id())
            {
                return true;
            }
        }
        let effects = self.items[index].on_input(input.translated(rect.pos), self.tile);
        self.apply_effects(backend, theme, effects);
        true
    }

    pub fn extend_extra_poll_fds(&self, fds: &mut Vec<std::os::fd::RawFd>) {
        self.dockapps.extend_poll_fds(fds);
        fds.extend(
            self.items
                .iter()
                .filter_map(|item| item.remote().and_then(RemoteTile::poll_fd)),
        );
    }

    pub fn next_housekeeping_deadline(
        &self,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        self.items
            .iter()
            .filter_map(|item| {
                item.remote()
                    .and_then(|tile| tile.next_service_deadline(now))
            })
            .min()
    }

    fn service_dockapps(&mut self, theme: &Theme) {
        let now = std::time::Instant::now();
        // Once, before anything else in the pass, so every tile serviced
        // below — and every `Welcome` sent above — describes the same
        // dock. Recomputing the serialized theme only happens when it
        // actually changed; see `ThemeBroadcast`.
        self.dockapps.refresh_theme(self.tile, self.scale, theme);

        let socket_path = self.dockapps.socket_path().clone();
        // Taken out first so the mutable borrow of `dockapps` ends
        // before the shared one below begins; handed back at the end.
        let mut scratch = std::mem::take(self.dockapps.scratch());
        let admissions = self.dockapps.service(now);

        // Borrowed, never cloned. `theme_toml` is a few kilobytes and
        // this runs on the repaint thread, so a clone per pass
        // would be a quarter of a megabyte a second of copying to
        // produce a value that is almost always identical. Disjoint
        // field borrows (`dockapps` shared, `items` mutable) are what
        // make that possible, and are why `admit` is a free function
        // over an iterator rather than a method on `&mut self`.
        let theme_state = self.dockapps.theme();
        for admission in admissions {
            dockapp::admit(
                self.items.iter_mut().filter_map(|item| item.remote_mut()),
                admission,
                theme_state,
                now,
            );
        }
        // The largest panel *content* the workarea beside the dock can
        // hold: everything left of the dock's column, minus the chrome
        // the shell will draw around the grant. Recomputed per pass for
        // the same reason the tile edge is — a monitor change moves it.
        let workarea = self.primary_workarea();
        let chrome = instrument::chrome_inset(theme) * 2;
        let panel_bounds = (
            workarea.size.w.saturating_sub(chrome),
            workarea.size.h.saturating_sub(chrome),
        );
        let mut ctx = ServiceContext {
            now,
            theme: theme_state,
            socket_path: &socket_path,
            scratch: &mut scratch,
            panel_bounds,
            // A dockapp on a hidden Dock is drawing frames nothing will
            // ever blit, and the protocol already has the word for it:
            // `Visibility { visible: false }` means "stop sampling and
            // stop drawing", not merely "nobody is looking".
            visible: true,
        };
        for item in &mut self.items {
            // A tile the supervisor evicted is one the dock has
            // disowned, and continuing to run its process would leave a
            // dockapp drawing frames nobody will ever blit. Shut it
            // down once; `shut_down` is idempotent.
            if item.evicted() {
                if let Some(tile) = item.remote_mut() {
                    if !matches!(
                        tile.state(),
                        TileState::Stopped {
                            reason: StopReason::Removed
                        }
                    ) {
                        tracing::warn!(id = %tile.id(), "shutting down an evicted dockapp");
                        tile.shut_down(chonk_dock_proto::wire::GoodbyeReason::Removed);
                    }
                }
                continue;
            }
            if let Some(tile) = item.remote_mut() {
                tile.service(&mut ctx);
            }
        }
        // Hand the buffer back so the next pass reuses the same
        // allocation rather than asking the allocator for a quarter of
        // a megabyte on the repaint thread.
        *self.dockapps.scratch() = scratch;
    }

    fn sync_instrument_panel(&mut self, backend: &mut B, theme: &Theme) {
        let now = std::time::Instant::now();
        // Exactly one panel desktop-wide: the last open this pass wins.
        let mut winner: Option<String> = None;
        for item in &mut self.items {
            if let Some(tile) = item.remote_mut() {
                if tile.take_panel_just_opened() {
                    winner = Some(tile.id().to_string());
                }
            }
        }
        // A built-in open that raced a remote one inside the same pass
        // pass wins the tie: the right-click is the user's own hand on
        // the tile this instant, where a remote open is a client
        // reacting to an earlier click. Either way the loser is closed
        // below — opening one kind of panel closes the other.
        if let Some(panel) = &mut self.builtin_panel {
            if std::mem::take(&mut panel.just_opened) {
                winner = Some(panel.id.clone());
            }
        }
        if let Some(winner) = &winner {
            for item in &mut self.items {
                if let Some(tile) = item.remote_mut() {
                    if tile.id() != winner && tile.panel_open() {
                        tile.close_panel(PanelCloseReason::Dismissed, now);
                    }
                }
            }
            if self
                .builtin_panel
                .as_ref()
                .is_some_and(|panel| panel.id != *winner)
            {
                self.builtin_panel = None;
            }
        }
        // A built-in owner the supervisor has since evicted (or that
        // somehow left the column) takes its panel state with it — the
        // no-owner arm below then hides the surface, exactly as a dead
        // dockapp's teardown does for a remote panel.
        if let Some(panel) = &self.builtin_panel {
            let live = self
                .items
                .iter()
                .any(|item| item.id() == panel.id && !item.evicted());
            if !live {
                self.builtin_panel = None;
            }
        }
        self.sync_builtin_sampling();

        // Whoever holds an open grant now owns the screen (at most one,
        // by the arbitration above).
        let owner = self
            .builtin_panel
            .as_ref()
            .map(|panel| panel.id.clone())
            .or_else(|| {
                self.items.iter().find_map(|item| {
                    item.remote()
                        .filter(|tile| tile.panel_open())
                        .map(|tile| tile.id().to_string())
                })
            });
        let Some(owner) = owner else {
            if self.instrument_panel.visible() {
                self.instrument_panel.hide(backend);
            }
            self.sync_escape_key_grab(backend);
            return;
        };

        // Geometry: beside the dock, level with the owning tile's slot.
        let Some((index, slot_top)) = self
            .item_slots()
            .find(|(index, _)| self.items[*index].id() == owner)
            .map(|(index, rect)| (index, rect.pos.y))
        else {
            // The tile left the column while holding a grant (removed
            // mid-pass); its shutdown path closes the panel, and the
            // next pass lands in the no-owner arm above.
            return;
        };

        // What was granted and whether new pixels are due this pass —
        // answered per kind, presented through the one surface below.
        // A built-in is polled while its panel is open (the direct
        // replacement for the frames a dockapp would push), so its
        // panel data rides the same pass that folds its samples.
        let (granted, ready) = if self.builtin_panel.is_some() {
            let ticked = self.items[index].panel_tick(now);
            // Cold panel sources may populate rows after the first grant.
            // Reuse the protocol's workarea/size bounds, and allocate only
            // when the granted dimensions actually change.
            let requested = if ticked {
                let workarea = self.primary_workarea();
                let chrome = instrument::chrome_inset(theme) * 2;
                let bounds = (
                    workarea.size.w.saturating_sub(chrome),
                    workarea.size.h.saturating_sub(chrome),
                );
                self.items[index]
                    .panel_spec(self.tile)
                    .and_then(|spec| clamp_panel_grant(spec.width, spec.height, bounds))
            } else {
                None
            };
            let Some(panel) = self.builtin_panel.as_mut() else {
                return;
            };
            if let Some(granted) = requested.filter(|&granted| granted != panel.granted) {
                panel.granted = granted;
                panel.frame = PanelFrame::new(granted.0, granted.1);
                panel.dirty = true;
            }
            let ready = std::mem::take(&mut panel.dirty) || ticked;
            (panel.granted, ready)
        } else {
            let Some(tile) = self.items[index].remote_mut() else {
                return;
            };
            let Some(granted) = tile.panel_granted() else {
                return;
            };
            (granted, tile.take_panel_ready(now))
        };

        let dock_geom = self.dock_geom();
        let inset = instrument::chrome_inset(theme);
        let geometry =
            instrument::place(granted, inset, dock_geom, slot_top, self.primary_workarea());

        let restage = !self.instrument_panel.visible()
            || self.instrument_panel.owner() != Some(owner.as_str())
            || self.instrument_panel.geometry() != geometry;
        if !restage && !ready {
            return;
        }

        // A built-in's frame is rendered here, on demand — the buffer
        // handoff that stands where a remote panel's banded stream
        // stood. Split borrows: the widget lives in `items`, its frame
        // in `builtin_panel`, the text machinery in `fonts` — disjoint
        // fields of `self`.
        if let Some(panel) = self.builtin_panel.as_mut() {
            let mut fonts = self.fonts.system();
            let mut swash = self.fonts.swash();
            let mut ctx = PanelCtx {
                theme,
                tile: self.tile,
                fonts: &mut fonts,
                swash: &mut swash,
            };
            self.items[index].render_panel(&mut panel.frame, &mut ctx);
        }
        // Split borrows again: the pixels live in `builtin_panel` or in
        // `items` (remote), the surface in `instrument_panel`.
        let frame = match self.builtin_panel.as_ref() {
            Some(panel) => Some(panel.frame.buffer()),
            None => self
                .items
                .iter()
                .find_map(|item| item.remote().filter(|tile| tile.id() == owner))
                .and_then(RemoteTile::panel_frame),
        };
        if restage {
            self.instrument_panel
                .show(backend, theme, &owner, granted, geometry, frame);
            self.sync_escape_key_grab(backend);
        } else {
            self.instrument_panel.repaint(backend, theme, frame);
        }
    }

    pub fn toggle_builtin_panel(&mut self, backend: &mut B, theme: &Theme, local: Point) -> bool {
        let Some(index) = self.item_index_at(local) else {
            return false;
        };
        let id = self.items[index].id().to_string();
        if self
            .builtin_panel
            .as_ref()
            .is_some_and(|panel| panel.id == id)
        {
            // The re-click is the toggle, resolved through the same
            // funnel every other dismissal gesture ends in.
            self.dismiss_instrument_panel(backend, PanelCloseReason::Dismissed);
            return true;
        }
        // The gesture is also a click-away: a right-click on any *other*
        // tile takes the open built-in panel down, exactly as a left
        // press on another tile does (`dock_input`). Done before the
        // capability check, because a panel-less tile is a place to
        // click away to just as much as a panel-capable one is —
        // otherwise the panel would survive a click on the tile right
        // next to it and look stuck.
        if self.builtin_panel.is_some() {
            self.dismiss_instrument_panel(backend, PanelCloseReason::Dismissed);
        }
        let Some(spec) = self.items[index].panel_spec(self.tile) else {
            return false;
        };
        // The same clamp a remote `OpenPanel` is granted through, so a
        // built-in cannot hold a panel a dockapp would be refused; the
        // same refusal for a degenerate spec or workarea.
        let workarea = self.primary_workarea();
        let chrome = instrument::chrome_inset(theme) * 2;
        let bounds = (
            workarea.size.w.saturating_sub(chrome),
            workarea.size.h.saturating_sub(chrome),
        );
        let Some(granted) = clamp_panel_grant(spec.width, spec.height, bounds) else {
            tracing::warn!(%id, asked = format!("{}x{}", spec.width, spec.height), "refusing a built-in instrument panel spec");
            return false;
        };
        self.builtin_panel = Some(BuiltinPanel {
            id,
            granted,
            frame: PanelFrame::new(granted.0, granted.1),
            just_opened: true,
            dirty: true,
        });
        self.sync_builtin_sampling();
        true
    }

    fn apply_panel_reaction(&mut self, reaction: PanelReaction) {
        match reaction {
            PanelReaction::None => return,
            PanelReaction::Repaint => {
                if let Some(panel) = &mut self.builtin_panel {
                    panel.dirty = true;
                }
                return;
            }
            PanelReaction::Close => {
                self.builtin_panel = None;
                self.sync_builtin_sampling();
                return;
            }
            // Both Run arities flatten through `effects()`, so the
            // single-command shorthand and the multi-command form
            // cannot drift apart — see `PanelReaction`.
            PanelReaction::Run(_) | PanelReaction::RunAll(_) => {}
        }
        // The identical executor a tile click's effects run on — see
        // `apply_effects` — which is the panel's whole safety story:
        // a panel action can only *be* one of these, and the dock, not
        // the widget, is what runs it. The commands go out in the
        // order the widget listed them, sequentially, on one thread.
        let (commands, repaint) = self.take_commands(reaction.effects());
        // A widget that answers a *panel* event with `Effect::Repaint`
        // means the panel; the dock's tiles repaint from `update`,
        // which sees whatever an action's resample brings back.
        if let Some(panel) = &mut self.builtin_panel {
            panel.dirty |= repaint;
        }
        run_detached(commands, self.launch_env());
        self.sync_builtin_sampling();
    }

    fn sync_escape_key_grab(&mut self, backend: &mut B) {
        let wanted = self.instrument_panel.visible();
        if wanted == self.escape_key_grabbed {
            return;
        }

        if wanted {
            backend.panel_keyboard(true);
        } else {
            backend.panel_keyboard(false);
        }
        self.escape_key_grabbed = wanted;
    }

    pub fn instrument_panel_visible(&self) -> bool {
        self.instrument_panel.visible()
    }

    pub fn instrument_panel_owns(&self, surface: B::ShellId) -> bool {
        self.instrument_panel.owns(surface)
    }

    pub fn dismiss_instrument_panel(&mut self, backend: &mut B, reason: PanelCloseReason) {
        let now = std::time::Instant::now();
        for item in &mut self.items {
            if let Some(tile) = item.remote_mut() {
                if tile.panel_open() {
                    tile.close_panel(reason, now);
                }
            }
        }
        // A built-in owner has no one to notify: dropping the state is
        // the whole close.
        self.builtin_panel = None;
        self.sync_builtin_sampling();
        if self.instrument_panel.visible() {
            self.instrument_panel.hide(backend);
        }
        self.sync_escape_key_grab(backend);
    }

    pub fn instrument_panel_click(
        &mut self,
        theme: &Theme,
        local: Point,
        button: wm_core::MouseButton,
        pressed: bool,
    ) {
        let Some(point) = self.instrument_panel.content_point(theme, local) else {
            return;
        };
        let Some(button) = reserved_filter(button) else {
            return;
        };
        let kind = if pressed {
            InputKind::Press
        } else {
            InputKind::Release
        };
        let event = InputEvent {
            kind,
            button: Some(button),
            x: point.x,
            y: point.y,
            delta: 0,
        };
        self.deliver_panel_event(event);
    }

    pub fn instrument_panel_scroll(&mut self, theme: &Theme, local: Point, step: i32) {
        let Some(point) = self.instrument_panel.content_point(theme, local) else {
            return;
        };
        let event = InputEvent {
            kind: InputKind::Scroll,
            button: None,
            x: point.x,
            y: point.y,
            delta: step,
        };
        self.deliver_panel_event(event);
    }

    pub fn update_panel_hover(&mut self, theme: &Theme, root: Point) {
        if !self.instrument_panel.visible() {
            return;
        }
        let geometry = self.instrument_panel.geometry();
        let local = Point::new(root.x - geometry.pos.x, root.y - geometry.pos.y);
        let content = self.instrument_panel.content_point(theme, local);
        let inside = content.is_some();
        if inside != self.instrument_panel.hovered() {
            self.instrument_panel.set_hovered(inside);
            let kind = if inside {
                InputKind::Enter
            } else {
                InputKind::Leave
            };
            self.deliver_panel_event(InputEvent {
                kind,
                button: None,
                x: 0,
                y: 0,
                delta: 0,
            });
        }
        // `Motion`, the panel-only kind: the pointer's position inside
        // the content, throttled by construction — this method runs
        // once per coalesced motion dispatch, and a stationary pointer
        // is deduplicated entirely.
        if let Some(point) = content {
            if self.instrument_panel.note_motion(point) {
                self.deliver_panel_event(InputEvent {
                    kind: InputKind::Motion,
                    button: None,
                    x: point.x,
                    y: point.y,
                    delta: 0,
                });
            }
        }
    }

    fn deliver_panel_event(&mut self, event: InputEvent) {
        let Some(owner) = self.instrument_panel.owner().map(str::to_string) else {
            return;
        };
        // A built-in owner hears the SDK vocabulary and answers with a
        // reaction, applied here; a remote one hears the wire shape
        // unchanged. One routing above this line, so click, scroll and
        // hover cannot treat the two kinds differently.
        if self
            .builtin_panel
            .as_ref()
            .is_some_and(|panel| panel.id == owner)
        {
            let Some(index) = self.items.iter().position(|item| item.id() == owner) else {
                return;
            };
            let reaction = self.items[index].panel_input(builtin_panel_event(&event), self.tile);
            self.apply_panel_reaction(reaction);
            return;
        }
        let now = std::time::Instant::now();
        if let Some(tile) = self
            .items
            .iter_mut()
            .filter_map(|item| item.remote_mut())
            .find(|tile| tile.id() == owner)
        {
            tile.panel_input(event, now);
        }
    }

    pub fn update_dock_hover(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        let dock = self.dock_geom();
        let inside =
            (dock.contains(root)).then(|| Point::new(root.x - dock.pos.x, root.y - dock.pos.y));
        let target = inside
            .and_then(|local| self.item_index_at(local))
            .map(|index| self.items[index].id().to_string());
        if target == self.hovered_item {
            return;
        }
        let left = self.hovered_item.take();
        if let Some(id) = left {
            let effects = self.deliver_by_id(&id, DockInput::Leave);
            self.apply_effects(backend, theme, effects);
        }
        if let Some(id) = target {
            let effects = self.deliver_by_id(&id, DockInput::Enter);
            self.apply_effects(backend, theme, effects);
            self.hovered_item = Some(id);
        }
    }

    fn deliver_by_id(&mut self, id: &str, input: DockInput) -> Vec<Effect> {
        let Some(index) = self.items.iter().position(|item| item.id() == id) else {
            return Vec::new();
        };
        self.items[index].on_input(input, self.tile)
    }

    pub fn shut_down_dockapps(&mut self, farewell: Farewell) {
        let handoff = self.dockapps.handoff_path();
        dockapp::shut_down(
            self.items.iter_mut().filter_map(|item| item.remote_mut()),
            handoff.as_deref(),
            farewell,
        );
    }

    fn apply_effects(&mut self, backend: &mut B, theme: &Theme, effects: Vec<Effect>) {
        let (commands, repaint) = self.take_commands(effects);
        run_detached(commands, self.launch_env());
        if repaint {
            self.redraw_dock(backend, theme);
        }
    }

    fn launch_env(&self) -> Vec<(String, String)> {
        crate::startup::launch_env(&self.theme_id, Some(self.appearance), self.scale)
    }

    #[allow(clippy::type_complexity)] // Ordered commands and their optional resampling handles.
    fn take_commands(
        &mut self,
        effects: Vec<Effect>,
    ) -> (
        Vec<(
            &'static str,
            Vec<String>,
            Option<crate::widgets::sampling::Resampler>,
        )>,
        bool,
    ) {
        let mut commands = Vec::new();
        let mut repaint = false;
        for effect in effects {
            match effect {
                Effect::Repaint => repaint = true,
                Effect::Resample(id) => {
                    if let Some(resampler) = self.samplers.resampler(id) {
                        resampler.resample_soon();
                    }
                }
                Effect::Run {
                    program,
                    args,
                    then,
                } => {
                    commands.push((
                        program,
                        args,
                        then.and_then(|id| self.samplers.resampler(id)),
                    ));
                }
            }
        }
        (commands, repaint)
    }

    fn redraw_dock(&mut self, backend: &mut B, theme: &Theme) {
        // A hidden Dock is not drawn, and its built-in samplers are
        // paused. Remote lifecycle work still runs while hidden, but
        // composing a column nobody can see has no reader.
        // `set_dock_visibility` calls straight back into here
        // on the way up, so the first frame after a show is composed
        // against the current geometry and palette rather than
        // whatever the column held when it went away.
        let dock_geom = self.dock_geom();
        let dock_height = dock_geom.size.h;
        backend.configure_shell_surface(self.dock_window, dock_geom);

        let Some(mut pixmap) = Pixmap::new(self.dock_width, dock_height.max(1)) else {
            return;
        };
        // The tile stack covers the whole column — the identity tile
        // plus every widget slot sums to at least `dock_height`, since
        // `stacked_dock_height`'s clamp only ever shortens it — so no
        // flat filler should ever be visible between tiles. The base
        // coat is still painted with the tile *face* rather than a
        // solid color, so that even a defensive gap (say, a widget
        // rendering short) reads as tile family, not as a hole in it.
        // `wallpaper.dock_color` stays only as the X11 window background
        // set at creation: the behind-everything fallback for the
        // instant before the first blit lands.
        paint::fill_area(
            &mut pixmap,
            0,
            0,
            self.dock_width,
            dock_height,
            &theme.tile.fill,
        );

        // Identity tile: flush at the dock's top-left corner, on the
        // same tile face/relief as every other square surface (it used
        // to borrow the titlebar's fill and bevel, which made the top of
        // the dock read as window chrome rather than as the column's
        // first tile), with the ChonkStep mark composited centered on
        // top — the mark is deliberately bold enough to survive the
        // Dock's original 56-pixel scale.
        tile::draw_tile_base(&mut pixmap, 0, 0, self.tile, theme);
        let logo_inset = (self.tile / 9).max(2);
        let logo_size = self.tile.saturating_sub(logo_inset * 2);
        // Resample once per tile size rather than per repaint, then blit
        // at 1:1 — see `scale_mark` for why not to let the blit scale it.
        if self
            .logo_scaled
            .as_ref()
            .is_none_or(|(size, _)| *size != logo_size)
        {
            self.logo_scaled = scale_mark(&self.logo, logo_size).map(|art| (logo_size, art));
        }
        let mark = self.logo_scaled.as_ref().map_or(&self.logo, |(_, art)| art);
        let logo_scale = logo_size as f32 / mark.width() as f32;
        let logo_paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        pixmap.draw_pixmap(
            0,
            0,
            mark.as_ref(),
            &logo_paint,
            Transform::from_row(
                logo_scale,
                0.0,
                0.0,
                logo_scale,
                logo_inset as f32,
                logo_inset as f32,
            ),
            None,
        );

        // The column is clipped to the usable output height. Fully
        // clipped faces cannot contribute a pixel, but their updates
        // and remote lifecycle still run in `tick_items`. A partially
        // visible tile must render normally; blitting clips its bottom.
        let visible_slots: Vec<_> = self
            .item_slots()
            .take_while(|(_, rect)| rect.pos.y < dock_height as i32)
            .collect();
        for (index, rect) in visible_slots {
            // `None` is an evicted widget: the dock draws its own
            // tombstone rather than calling code it has already
            // disowned. `render_dead_tile` is the same powered-off
            // face the instruments already use for "no sink", "no
            // interface", "no battery" — an evicted slot should read as
            // a dead instrument, which belongs to the family, and not
            // as a hole punched in the column. The widget's own name is
            // the label, so the dock says *which* one went dark without
            // the user having to find the log.
            let buffer = match self.items[index].render(
                theme,
                self.tile,
                &mut self.fonts.system(),
                &mut self.fonts.swash(),
            ) {
                Some(buffer) => buffer,
                None => {
                    let label = self.items[index].name();
                    panel::render_dead_tile(
                        theme,
                        &mut self.fonts.system(),
                        &mut self.fonts.swash(),
                        self.tile,
                        label,
                    )
                }
            };
            blit_into(&mut pixmap, rect.pos.x as u32, rect.pos.y as u32, &buffer);

            if self.item_drag.as_ref().is_some_and(|d| d.index == index) {
                // "You've picked this up": brighten the slot's outermost
                // pixel ring with the relief's own +80 light delta
                // (`tile::op_line`, relative) rather than stamping an
                // absolute chrome color over it — the tile's RAISED2
                // edge stays structurally intact and simply reads as
                // lit, so the pickup highlight speaks the tile family's
                // language instead of borrowing the titlebar bevel's.
                // Drawn on the edge itself rather than in a surrounding
                // gap: tiles snap together, so there's no gap to use.
                let (x, y) = (rect.pos.x, rect.pos.y);
                let (w, h) = (rect.size.w as i32, rect.size.h as i32);
                tile::op_line(&mut pixmap, x, y, x + w - 1, y, 80);
                tile::op_line(&mut pixmap, x, y + h - 1, x + w - 1, y + h - 1, 80);
                tile::op_line(&mut pixmap, x, y, x, y + h - 1, 80);
                tile::op_line(&mut pixmap, x + w - 1, y, x + w - 1, y + h - 1, 80);
            }
        }

        backend.paint_shell_surface(self.dock_window, &pixmap_to_buffer(pixmap));
    }
}

impl<B: Backend> Drop for Desktop<B> {
    fn drop(&mut self) {
        self.shut_down_dockapps(Farewell::SessionOver);
    }
}
impl<B: Backend> Desktop<B> {
    pub fn remote_menu(&self, local: Point) -> Option<(String, Vec<wm_theme::menu::MenuItem>)> {
        let tile = self.items[self.item_index_at(local)?].remote()?;
        Some((
            tile.id().to_string(),
            vec![
                wm_theme::menu::MenuItem::Action {
                    label: format!("{} — {:?}", tile.name(), tile.state()),
                    action: 0,
                },
                wm_theme::menu::MenuItem::Action {
                    label: "Restart".into(),
                    action: 1,
                },
                wm_theme::menu::MenuItem::Action {
                    label: "Remove for this run".into(),
                    action: 2,
                },
            ],
        ))
    }
    pub fn remote_action(&mut self, backend: &mut B, theme: &Theme, id: &str, action: u32) {
        let Some(index) = self.items.iter().position(|item| item.id() == id) else {
            return;
        };
        if let Some(tile) = self.items[index].remote_mut() {
            match action {
                1 => tile.user_restart(std::time::Instant::now()),
                2 => tile.shut_down(chonk_dock_proto::wire::GoodbyeReason::Removed),
                _ => return,
            }
        }
        if action == 2 {
            self.items.remove(index);
            self.persist_order();
        }
        self.redraw_dock(backend, theme);
    }
}
#[cfg(test)]
mod tests {
    use super::dock_order::{arrange, load, merge, save};
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    #[test]
    fn builtin_panel_events_translate_the_wire_vocabulary_faithfully() {
        let event = |kind, delta| InputEvent {
            kind,
            button: None,
            x: 3,
            y: 4,
            delta,
        };
        let at = Point::new(3, 4);
        assert_eq!(
            builtin_panel_event(&event(InputKind::Press, 0)),
            PanelEvent::LeftPress { local: at }
        );
        assert_eq!(
            builtin_panel_event(&event(InputKind::Release, 0)),
            PanelEvent::LeftRelease { local: at }
        );
        assert_eq!(
            builtin_panel_event(&event(InputKind::Scroll, -2)),
            PanelEvent::Scroll {
                local: at,
                delta: -2
            }
        );
        assert_eq!(
            builtin_panel_event(&event(InputKind::Motion, 0)),
            PanelEvent::Motion { local: at }
        );
        assert_eq!(
            builtin_panel_event(&event(InputKind::Enter, 0)),
            PanelEvent::Enter
        );
        assert_eq!(
            builtin_panel_event(&event(InputKind::Leave, 0)),
            PanelEvent::Leave
        );
    }
    fn ids(order: &[&str]) -> Vec<String> {
        order.iter().map(|id| id.to_string()).collect()
    }
    fn arranged(items: &[&str], order: &[String]) -> Vec<String> {
        arrange(ids(items), order, |item| item.clone())
    }
    fn temp_state_file(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!(
                "chonkstep-dock-order-{}-{tag}-{unique}",
                std::process::id()
            ))
            .join("dock-items")
    }
    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }
    #[test]
    fn a_session_with_no_remembered_order_keeps_the_built_in_default() {
        assert_eq!(arranged(&["a", "b", "c"], &[]), ids(&["a", "b", "c"]));
    }
    #[test]
    fn the_remembered_order_is_what_the_column_comes_up_in() {
        assert_eq!(
            arranged(&["a", "b", "c"], &ids(&["c", "a", "b"])),
            ids(&["c", "a", "b"])
        );
    }
    #[test]
    fn an_item_nobody_remembers_lands_at_the_bottom_in_declaration_order() {
        assert_eq!(
            arranged(&["a", "b", "new1", "new2"], &ids(&["b", "a"])),
            ids(&["b", "a", "new1", "new2"])
        );
    }
    #[test]
    fn an_entry_that_did_not_resolve_keeps_its_place_through_a_reorder() {
        let remembered = ids(&["clock", "absent", "power"]);
        // It resolves to nothing, so the live column has two tiles.
        assert_eq!(
            arranged(&["clock", "power"], &remembered),
            ids(&["clock", "power"])
        );
        // The user then drags power above clock. The absent entry is
        // still between them, because that is where they left it.
        assert_eq!(
            merge(&ids(&["power", "clock"]), &remembered),
            ids(&["power", "clock", "absent"])
        );
        // ...and with the original arrangement it is still in the
        // middle, following the same neighbour it always followed.
        assert_eq!(
            merge(&ids(&["clock", "power"]), &remembered),
            ids(&["clock", "absent", "power"])
        );
    }
    #[test]
    fn an_unresolved_first_entry_goes_back_to_the_top() {
        assert_eq!(
            merge(&ids(&["b", "c"]), &ids(&["absent", "b", "c"])),
            ids(&["absent", "b", "c"])
        );
    }
    #[test]
    fn consecutive_unresolved_entries_keep_their_own_order() {
        assert_eq!(
            merge(&ids(&["a", "z"]), &ids(&["a", "x", "y", "z"])),
            ids(&["a", "x", "y", "z"])
        );
    }
    #[test]
    fn the_order_round_trips_through_the_state_file() {
        let path = temp_state_file("roundtrip");
        save(
            &path,
            &ids(&["builtin:clock", "chonk-dockclock", "builtin:net"]),
        )
        .unwrap();
        assert_eq!(
            load(&path),
            ids(&["builtin:clock", "chonk-dockclock", "builtin:net"])
        );

        std::fs::write(&path, "# hand-edited\n\nbuiltin:net\n  builtin:clock  \n").unwrap();
        assert_eq!(
            load(&path),
            ids(&["builtin:net", "builtin:clock"]),
            "comments and padding are not ids"
        );

        cleanup(&path);
    }
    #[test]
    fn a_missing_file_is_an_empty_order_rather_than_an_error() {
        assert!(load(Path::new("/nonexistent/chonkstep/dock-items")).is_empty());
    }
    #[test]
    fn a_duplicated_remembered_id_places_the_item_once() {
        assert_eq!(
            arranged(&["a", "b"], &ids(&["b", "b", "a"])),
            ids(&["b", "a"])
        );
    }
}
