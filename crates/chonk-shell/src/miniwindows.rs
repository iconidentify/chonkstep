//! Optional desktop previews, independent of the retired dock and its widgets.
use wm_core::{Backend, ClientId, DragHandle, Lifecycle, WindowManager};
use wm_theme::{FontState, Theme, UiChrome};
use wm_theme_api::{Point, Rect, Size};

struct Tile<Id> {
    client: ClientId,
    surface: Id,
    rect: Rect,
    manual: bool,
    title: String,
    mapped: bool,
    missing_preview: bool,
}
struct Drag<Id> {
    surface: Id,
    offset: Point,
    start: Point,
    moved: bool,
    handle: DragHandle,
}
pub(crate) struct Miniwindows<B: Backend> {
    tiles: Vec<Tile<B::ShellId>>,
    drag: Option<Drag<B::ShellId>>,
    revision: Option<(u64, u64, u64, u32, bool)>,
}
impl<B: Backend> Default for Miniwindows<B> {
    fn default() -> Self { Self { tiles: Vec::new(), drag: None, revision: None } }
}
impl<B: Backend> Miniwindows<B> {
    pub fn invalidate(&mut self, backend: &mut B) {
        self.revision = None;
        if let Some(drag) = self.drag.take() { backend.ungrab_pointer(drag.handle); }
    }

    pub fn sync(&mut self, wm: &mut WindowManager<B>, enabled: bool, scale: f32,
        theme: &Theme, chrome: &UiChrome, fonts: &FontState) {
        if !enabled && self.tiles.is_empty() { return; }
        let previews = if self.tiles.iter().any(|tile| tile.missing_preview) {
            wm.backend().preview_generation()
        } else { 0 };
        let revision = (wm.protocol_state_revision(), wm.workarea_revision(),
            previews, scale.to_bits(), enabled);
        if self.revision.as_ref() == Some(&revision) { return; }
        let repaint = self.revision.as_ref().is_none_or(|old| old.3 != revision.3);
        self.revision = Some(revision);
        let clients: Vec<_> = wm.iter_clients().filter(|(_, c)| enabled && c.lifecycle == Lifecycle::Miniaturized)
            .map(|(id, c)| (id, c.title.clone(), c.workspace)).collect();
        let dead: Vec<_> = self.tiles.iter().filter(|tile| !clients.iter().any(|(id, _, _)| *id == tile.client))
            .map(|tile| tile.surface).collect();
        for surface in dead {
            if self.drag.as_ref().is_some_and(|drag| drag.surface == surface) {
                if let Some(drag) = self.drag.take() { wm.backend_mut().ungrab_pointer(drag.handle); }
            }
            wm.backend_mut().destroy_shell_surface(surface);
            self.tiles.retain(|tile| tile.surface != surface);
        }
        let edge = (160.0 * scale).round().max(32.0) as u32;
        let pad = (12.0 * scale).round().max(4.0) as u32;
        for (id, title, workspace) in clients {
            let output = wm.workspace_output_index(workspace).unwrap_or(0);
            let Some(area) = wm.monitors_ref().get(output).map(|m| wm.usable_area_at(m.geometry.pos)) else { continue; };
            let size = edge.min(area.size.w.saturating_sub(pad * 2)).min(area.size.h.saturating_sub(pad * 2)).max(1);
            let visible = wm.workspace_visible(workspace);
            let index = self.tiles.iter().position(|tile| tile.client == id);
            let slot = index.unwrap_or(self.tiles.len());
            let columns = (area.size.w.saturating_sub(pad) / (size + pad)).max(1);
            let mut rect = Rect::new(Point::new(
                area.pos.x + (pad + slot as u32 % columns * (size + pad)) as i32,
                area.pos.y + area.size.h as i32 - (pad + size + slot as u32 / columns * (size + pad)) as i32,
            ), Size::new(size, size));
            if let Some(tile) = index.map(|i| &self.tiles[i]).filter(|t| t.manual) { rect.pos = tile.rect.pos; }
            rect.pos.x = rect.pos.x.clamp(area.pos.x, area.pos.x + area.size.w.saturating_sub(size) as i32);
            rect.pos.y = rect.pos.y.clamp(area.pos.y, area.pos.y + area.size.h.saturating_sub(size) as i32);
            let fresh = index.is_none();
            let index = if let Some(index) = index { index } else {
                let Some(surface) = wm.backend_mut().create_shell_surface(rect, (0, 0, 0), false) else { continue; };
                self.tiles.push(Tile { client: id, surface, rect, manual: false, title: String::new(), mapped: false, missing_preview: true });
                self.tiles.len() - 1
            };
            let tile = &mut self.tiles[index];
            if repaint || fresh || tile.missing_preview || tile.rect.size != rect.size || tile.title != title {
                let preview = wm.client_preview(id);
                tile.missing_preview = preview.is_none();
                let buffer = chrome.icon(theme, &mut fonts.system(), &mut fonts.swash(), size, &title, preview.as_ref());
                wm.backend_mut().paint_shell_surface(tile.surface, &buffer);
                tile.title = title;
            }
            if tile.rect != rect { wm.backend_mut().configure_shell_surface(tile.surface, rect); tile.rect = rect; }
            if tile.mapped != visible {
                if visible { wm.backend_mut().map_shell_surface(tile.surface); }
                else { wm.backend_mut().unmap_shell_surface(tile.surface); }
                tile.mapped = visible;
            }
        }
    }
    pub fn press(&mut self, backend: &mut B, surface: B::ShellId, offset: Point) -> bool {
        let Some(tile) = self.tiles.iter().find(|tile| tile.surface == surface) else { return false; };
        self.drag = Some(Drag { surface, offset, start: tile.rect.pos, moved: false, handle: backend.grab_pointer_for_drag() });
        true
    }
    pub fn motion(&mut self, backend: &mut B, root: Point) {
        let Some(drag) = &mut self.drag else { return; };
        let next = Point::new(root.x - drag.offset.x, root.y - drag.offset.y);
        if !drag.moved && (next.x - drag.start.x).abs().max((next.y - drag.start.y).abs()) < 6 { return; }
        drag.moved = true;
        if let Some(tile) = self.tiles.iter_mut().find(|tile| tile.surface == drag.surface) {
            tile.rect.pos = next;
            tile.manual = true;
            backend.configure_shell_surface(tile.surface, tile.rect);
        }
    }
    pub fn release(&mut self, backend: &mut B) -> Option<ClientId> {
        let drag = self.drag.take()?;
        backend.ungrab_pointer(drag.handle);
        self.tiles.iter().find(|tile| tile.surface == drag.surface && !drag.moved).map(|tile| tile.client)
    }
}
