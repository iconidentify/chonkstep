//! The launcher dock: the defining feature of the classic NeXTSTEP
//! desktop, a strip of pinnable application tiles down the left edge
//! below the Clip. Pin by dragging a miniaturized window's icon tile
//! onto the strip (the shell resolves the window back to its
//! application through the `.desktop` index), click to launch — or to
//! focus the running window when one exists — and drag a tile off the
//! strip to unpin. Pins persist across sessions in the state directory.
//!
//! The strip is one shell surface (like the Dock and the Clip in
//! `desktop.rs`), exactly one tile wide, starting at `y = tile`
//! directly below the Clip: the Clip opens the left edge the way the
//! identity tile opens the right one, and the launcher stacks beneath
//! it, tiles touching, in the one flush column the classic NeXTSTEP
//! desktop draws. Its faces come from
//! `wm_theme::launcher::render_launcher_tile`, so a pinned app renders
//! in the same tile grammar as every other square surface.
//!
//! Persistence is the same one-file state mechanism `theme_select.rs`
//! and `wallpaper.rs` use — `$XDG_STATE_HOME/chonkstep/dock` (or the
//! `~/.local/state` fallback), one desktop-file id per line. Ids are
//! resolved against the scanned application index at load; an id whose
//! `.desktop` entry no longer exists is dropped with a warning, and
//! the file is rewritten on the next mutation rather than at load —
//! loading is a read, and a transiently unscanned app (a broken
//! `.desktop` edit, an unmounted overlay) should not have its pin
//! erased just for being unresolvable once.

use std::path::{Path, PathBuf};

use wm_core::{Backend, DragHandle};
use wm_theme::Theme;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::apps::{match_window_class, AppEntry};

/// What a click on the strip resolved to — the shell dispatches:
/// `Launch` spawns the entry, `Focus` activates the running window.
/// Generic over the backend's client-window id (`W` =
/// `Backend::WindowId` in the shell) rather than over the backend
/// itself: the enum only carries an id, and the free helpers below
/// (`resolve_click`, `running_lamps`) stay directly testable with
/// plain integers.
pub enum LaunchDockAction<W> {
    Launch(AppEntry),
    Focus(W),
}

/// A press on a strip tile, possibly mid-drag — the same
/// press-arms-both-click-and-drag shape as `desktop.rs`'s `IconDrag`,
/// resolved on release by whether the pointer ever crossed the
/// threshold.
struct StripDrag {
    /// The pressed tile's slot at press time — stable for the whole
    /// drag, since the strip only reorders on release.
    index: usize,
    /// Root position of the press, the threshold's fixed reference.
    press_root: Point,
    /// Crossed the drag threshold? A release that never did is a
    /// click — the same press-never-moved rule the icon tiles use.
    moved: bool,
    grab: DragHandle,
}

pub struct LaunchDock<B: Backend> {
    /// The strip's one shell surface — `None` until the first pin ever
    /// needs it, and kept (unmapped) across empty spells rather than
    /// destroyed, the same churn-avoidance reasoning as the switcher
    /// panel in `desktop.rs`.
    window: Option<B::ShellId>,
    mapped: bool,
    tile: u32,
    /// The monitor this strip lives on - the primary. Its position is
    /// what `strip_origin` anchors to; its height still bounds how
    /// many tiles fit.
    primary: Rect,
    /// Same ~4px-scaled threshold as `desktop.rs`'s `drag_threshold`,
    /// derived from the tile size (which is itself `56 * scale`) so
    /// the strip feels the same at any `CHONKSTEP_SCALE`.
    drag_threshold: i32,
    state_path: Option<PathBuf>,
    /// The pinned entries, top to bottom — resolved clones of the
    /// scanned index, so a tile can launch or match even if the index
    /// is rescanned behind it.
    pins: Vec<AppEntry>,
    /// Per-pin running lamp, parallel to `pins` — booleans rather than
    /// matched window ids because the lamp is all the cache drives;
    /// `Focus` resolution always reads the fresh `running` pairs the
    /// shell passes into `handle_click`.
    lit: Vec<bool>,
    /// A pin mutation made the current lamp vector provisional. The
    /// shell's client cache may still match perfectly after a new pin
    /// is inserted, so this independent bit guarantees the next tick
    /// reconciles that pin against the already-running applications.
    running_dirty: bool,
    drag: Option<StripDrag>,
    /// The session's one shared font database — see
    /// `wm_theme::FontState` ("call it once per session"); the strip
    /// used to scan its own.
    fonts: wm_theme::FontState,
}

impl<B: Backend> LaunchDock<B> {
    /// Loads persisted pins (resolving desktop-file ids against
    /// `apps`; stale ids are dropped with a warning) and creates the
    /// strip surface when there is anything to show.
    pub fn new(backend: &mut B, theme: &Theme, primary: Rect, tile: u32, apps: &[AppEntry], fonts: wm_theme::FontState) -> Self {
        let tile = tile.max(1);
        let state_path = state_path();
        let pins = state_path.as_deref().map(|path| load_pins(path, apps)).unwrap_or_default();
        let lit = vec![false; pins.len()];
        let mut dock = Self {
            window: None,
            mapped: false,
            tile,
            primary,
            drag_threshold: drag_threshold_for_tile(tile),
            state_path,
            pins,
            lit,
            running_dirty: true,
            drag: None,
            fonts,
        };
        dock.sync_window(backend, theme);
        dock
    }

    /// Whether `surface` is the strip — the shell routes its clicks
    /// and motion here when so.
    pub fn owns_window(&self, surface: B::ShellId) -> bool {
        self.window == Some(surface)
    }

    /// A button press/release on the strip. Press arms a potential
    /// drag (grabbing the pointer, like the icon tiles, so a fast drag
    /// can't outrun the narrow strip); a release that never crossed
    /// the drag threshold is a click, resolving to a
    /// [`LaunchDockAction`]: `Focus` when one of the `running`
    /// `(WM_CLASS class, client window id)` pairs matches the tile's
    /// entry, `Launch` otherwise. A release that did cross the
    /// threshold completes the drag instead — reorder on the strip,
    /// unpin off it.
    pub fn handle_click(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        local: Point,
        pressed: bool,
        running: &[(String, B::WindowId)],
    ) -> Option<LaunchDockAction<B::WindowId>> {
        let origin = strip_origin(self.primary, self.tile);
        let root = Point::new(local.x + origin.x, local.y + origin.y);
        if pressed {
            let slot = slot_at(origin, self.tile, self.pins.len(), root)?;
            // A leaked previous press (its release never reached us)
            // must give its grab back before the new one takes over.
            if let Some(stale) = self.drag.take() {
                backend.ungrab_pointer(stale.grab);
            }
            let grab = backend.grab_pointer_for_drag();
            self.drag = Some(StripDrag { index: slot, press_root: root, moved: false, grab });
            return None;
        }
        self.finish_drag(backend, theme, root, running)
    }

    /// Root-relative pointer motion while a strip drag may be in
    /// progress — call on every motion event, like the icon drags.
    /// The strip's drop feedback is deliberately light: crossing the
    /// threshold ghosts the picked-up tile in place (the drop target
    /// is always a strip slot or "off", so a tile chasing the pointer
    /// would add motion without adding information).
    pub fn handle_motion(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        {
            let Some(drag) = self.drag.as_mut() else { return };
            if drag.moved || !crossed_threshold(drag.press_root, root, self.drag_threshold) {
                return;
            }
            drag.moved = true;
        }
        self.repaint(backend, theme);
    }

    /// Ends an in-progress strip drag, if any: a drop back on the
    /// strip reorders the dragged tile to the slot under `root`, a
    /// drop off the strip unpins it — either way persisted. Returns
    /// whether the release was consumed. A press that never crossed
    /// the threshold is *not* consumed while the pointer is still over
    /// the strip: that release is a click, and it arrives (with the
    /// running-window pairs a click needs) through `handle_click`.
    pub fn handle_release(&mut self, backend: &mut B, theme: &Theme, root: Point) -> bool {
        let origin = strip_origin(self.primary, self.tile);
        let Some(drag) = self.drag.as_ref() else { return false };
        if !drag.moved {
            if slot_at(origin, self.tile, self.pins.len(), root).is_some() {
                return false;
            }
            // A sub-threshold press released off the strip (possible
            // only right at the strip's edge): a cancelled click.
            if let Some(drag) = self.drag.take() {
                backend.ungrab_pointer(drag.grab);
            }
            return true;
        }
        self.finish_drag(backend, theme, root, &[]);
        true
    }

    /// Repaints running-app indicators from the current set of managed
    /// windows' `(WM_CLASS class, client window id)` pairs — call from
    /// the shell's tick; a cheap no-op when no tile's lamp actually
    /// changed.
    pub fn update_running(&mut self, backend: &mut B, theme: &Theme, running: &[(String, B::WindowId)]) {
        let lit = running_lamps(&self.pins, running);
        self.running_dirty = false;
        if lit != self.lit {
            self.lit = lit;
            self.repaint(backend, theme);
        }
    }

    /// Whether the pin set changed since running lamps were last
    /// reconciled. Kept separate from the shell's client cache because
    /// pinning an application changes no client at all.
    pub fn running_refresh_needed(&self) -> bool {
        self.running_dirty
    }

    /// Attempts to pin `app` at the strip position under `root` — the
    /// drop half of dragging a miniwindow icon onto the strip. `false`
    /// when `root` isn't over the strip's pin zone (the strip's
    /// current extent, or the would-be first slot when nothing is
    /// pinned yet).
    pub fn try_pin_at(&mut self, backend: &mut B, theme: &Theme, root: Point, app: &AppEntry) -> bool {
        let origin = strip_origin(self.primary, self.tile);
        let Some(slot) = slot_at(origin, self.tile, self.pins.len().max(1), root) else {
            return false;
        };
        match self.pins.iter().position(|pin| pin.id == app.id) {
            Some(existing) => {
                // Already pinned: one tile per app is the classic dock
                // behavior, so a re-pin moves the existing tile to the
                // drop slot instead of duplicating it.
                let slot = slot.min(self.pins.len().saturating_sub(1));
                move_pin(&mut self.pins, existing, slot);
                move_pin(&mut self.lit, existing, slot);
            }
            None => {
                let slot = slot.min(self.pins.len());
                self.pins.insert(slot, app.clone());
                self.lit.insert(slot, false);
            }
        }
        self.running_dirty = true;
        self.persist();
        self.sync_window(backend, theme);
        true
    }

    /// Resolves the release that ends an armed press: a click when the
    /// threshold was never crossed, otherwise reorder-or-unpin by
    /// where the drop landed. The one shared endpoint for both
    /// `handle_click(pressed = false)` and `handle_release`, so the
    /// two entry points can never disagree about drop semantics.
    fn finish_drag(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        root: Point,
        running: &[(String, B::WindowId)],
    ) -> Option<LaunchDockAction<B::WindowId>> {
        let origin = strip_origin(self.primary, self.tile);
        let drag = self.drag.take()?;
        backend.ungrab_pointer(drag.grab);

        if !drag.moved {
            let pin = self.pins.get(drag.index)?;
            return Some(resolve_click(pin, running));
        }
        match slot_at(origin, self.tile, self.pins.len(), root) {
            Some(slot) => {
                move_pin(&mut self.pins, drag.index, slot);
                move_pin(&mut self.lit, drag.index, slot);
                self.running_dirty = true;
                self.persist();
                self.repaint(backend, theme);
            }
            None => {
                if drag.index < self.pins.len() {
                    self.pins.remove(drag.index);
                    self.lit.remove(drag.index);
                    self.running_dirty = true;
                    self.persist();
                    self.sync_window(backend, theme);
                }
            }
        }
        None
    }

    /// Writes the current pins to the state file — called on every
    /// mutation (pin, unpin, reorder), which is also what finally
    /// rewrites away any stale ids `load_pins` dropped.
    fn persist(&self) {
        let Some(path) = &self.state_path else { return };
        if let Err(error) = save_pins(path, &self.pins) {
            tracing::warn!(?error, "failed to persist launcher pins");
        }
    }

    /// Brings the strip surface in line with the pin count: sized
    /// `pins * tile` tall (screen-clamped like the Dock), resized and
    /// remapped on pin/unpin, unmapped entirely when no pins exist —
    /// and repainted whenever it is showing.
    /// Moves the strip onto `primary` - the monitor arrangement
    /// changed under it.
    ///
    /// Re-dresses the strip in `theme` and, if the UI scale moved,
    /// re-derives its tile edge — the live theme/scale entry point, and
    /// the counterpart to [`LaunchDock::reposition`]'s monitor-change
    /// one.
    ///
    /// Deliberately *not* routed through `reposition`: that method
    /// early-returns when the primary rect is unchanged, which is
    /// exactly the case here (a restyle moves no monitor), so reusing
    /// it would swallow every scale change that did not coincide with a
    /// display being plugged in. The repaint is unconditional for the
    /// same class of reason — a theme change moves no tile edge, and a
    /// method that only acted when the size changed would leave the
    /// strip wearing the old palette.
    pub fn restyle(&mut self, backend: &mut B, theme: &Theme, tile: u32) {
        let tile = tile.max(1);
        if self.tile != tile {
            self.tile = tile;
            self.drag_threshold = drag_threshold_for_tile(tile);
        }
        self.sync_window(backend, theme);
    }

    /// The strip anchors to the primary monitor rather than the
    /// desktop origin (see `strip_origin`), so the rect it was built
    /// with goes stale the moment a display is plugged in, unplugged,
    /// or the session is resized. Everything geometric here reads that
    /// stored rect - where the surface is configured, and where clicks
    /// are hit-tested - so a stale one detaches the strip from the
    /// Clip it sits below *and* sends slot hit-testing to coordinates
    /// nothing is drawn at.
    pub fn reposition(&mut self, backend: &mut B, theme: &Theme, primary: Rect) {
        if self.primary == primary {
            return;
        }
        self.primary = primary;
        self.sync_window(backend, theme);
    }

    fn sync_window(&mut self, backend: &mut B, theme: &Theme) {
        if self.pins.is_empty() {
            if self.mapped {
                if let Some(window) = self.window {
                    backend.unmap_shell_surface(window);
                    // The surface survives the last pin being removed —
                    // re-pinning should not churn the display server —
                    // but the strip's raster grows with the pin count
                    // and is rebuilt in full below the moment one comes
                    // back, so nothing is kept by keeping it.
                    backend.release_shell_buffer(window);
                }
                self.mapped = false;
            }
            return;
        }
        let geometry = Rect { pos: strip_origin(self.primary, self.tile), size: Size::new(self.tile, self.strip_height()) };
        let window = match self.window {
            Some(window) => {
                backend.configure_shell_surface(window, geometry);
                window
            }
            None => match backend.create_shell_surface(geometry, crate::desktop::DESKTOP_BG, true) {
                Some(window) => {
                    self.window = Some(window);
                    window
                }
                None => {
                    tracing::warn!("failed to create launcher strip surface");
                    return;
                }
            },
        };
        if !self.mapped {
            backend.map_shell_surface(window);
            self.mapped = true;
        }
        backend.raise_shell_surface(window);
        self.repaint(backend, theme);
    }

    /// The strip's visible height: every pinned tile, clamped to what
    /// actually fits below the Clip so an absurd pin count can't ask
    /// the backend for an invalid surface (same defensive clamp as the
    /// Dock's `stacked_dock_height`).
    fn strip_height(&self) -> u32 {
        let full = (self.pins.len() as u32).saturating_mul(self.tile);
        let below_clip = self.primary.size.h.saturating_sub(self.tile).max(self.tile);
        full.min(below_clip).max(1)
    }

    /// Renders every pin's tile into one strip buffer and paints it.
    /// The buffer is assembled by concatenation rather than through an
    /// intermediate image: tiles are exactly strip-wide, so each one
    /// *is* its band of rows.
    fn repaint(&mut self, backend: &mut B, theme: &Theme) {
        let Some(window) = self.window else { return };
        if self.pins.is_empty() {
            return;
        }
        let slot_bytes = (self.tile * self.tile * 4) as usize;
        let mut pixels = Vec::with_capacity(slot_bytes * self.pins.len());
        for (index, pin) in self.pins.iter().enumerate() {
            let lit = self.lit.get(index).copied().unwrap_or(false);
            let buffer = wm_theme::launcher::render_launcher_tile(
                theme,
                &mut self.fonts.system(),
                &mut self.fonts.swash(),
                self.tile,
                &pin.name,
                lit,
            );
            if buffer.pixels.len() == slot_bytes {
                pixels.extend_from_slice(&buffer.pixels);
            } else {
                // A failed tile render (zero-size pixmap) still holds
                // its slot so the tiles below it don't shift.
                pixels.resize(pixels.len() + slot_bytes, 0);
            }
        }
        if let Some(drag) = self.drag.as_ref().filter(|drag| drag.moved) {
            ghost_slot(&mut pixels, self.tile, drag.index);
        }
        backend.paint_shell_surface(window, &DecorationBuffer { width: self.tile, height: self.pins.len() as u32 * self.tile, pixels });
    }
}

/// How far a press must travel before it is a reorder drag rather than
/// a launch click, derived from the strip's own tile edge so the
/// gesture feels identical at any UI scale. Expressed in tiles rather
/// than in the scale directly because the strip is only ever told its
/// tile size — see `crate::desktop::drag_threshold_px`, the same
/// gesture measured from the other end.
fn drag_threshold_for_tile(tile: u32) -> i32 {
    ((4.0 * tile as f32 / 56.0).round() as i32).max(2)
}

/// Root position of the strip's top-left corner: the primary monitor's
/// own top-left corner.
///
/// This used to start one tile down, because the Clip owned the corner
/// above it and the strip hung off its bottom edge. The Clip has since
/// moved to the bottom-right (see `desktop::clip_geometry`), so holding
/// the old offset would leave an empty tile of desktop above the strip
/// — a gap shaped exactly like the thing that is no longer there.
/// Anchoring to the *monitor* rather than the desktop origin is what
/// keeps the strip on the primary in a multi-head layout - with a
/// second screen to the left of the primary the desktop origin is off
/// on that other monitor, and a root-anchored strip would sit there by
/// itself.
fn strip_origin(primary: Rect, _tile: u32) -> Point {
    primary.pos
}

/// Which of `slots` tile slots the root-relative `root` falls in, for
/// a one-tile-wide strip at `origin` — `None` off the strip's extent.
fn slot_at(origin: Point, tile: u32, slots: usize, root: Point) -> Option<usize> {
    if slots == 0 || tile == 0 {
        return None;
    }
    let (dx, dy) = (root.x - origin.x, root.y - origin.y);
    if dx < 0 || dx >= tile as i32 || dy < 0 {
        return None;
    }
    let slot = (dy / tile as i32) as usize;
    (slot < slots).then_some(slot)
}

/// Whether motion from `press` to `current` crossed the drag
/// threshold on either axis — the same per-axis `>=` shape as
/// `desktop.rs`'s `resolve_drag_position`, so a strip press and an
/// icon press turn into drags at exactly the same finger travel.
fn crossed_threshold(press: Point, current: Point, threshold: i32) -> bool {
    (current.x - press.x).abs() >= threshold || (current.y - press.y).abs() >= threshold
}

/// What a plain click on `pin`'s tile means right now: focus the
/// first running window that matches the entry, launch otherwise.
/// Matching goes through [`match_window_class`] over a one-entry
/// slice so the `StartupWMClass`-then-name-then-executable precedence
/// rules live in exactly one place.
fn resolve_click<W: Copy>(pin: &AppEntry, running: &[(String, W)]) -> LaunchDockAction<W> {
    for (class, window) in running {
        if match_window_class(std::slice::from_ref(pin), class).is_some() {
            return LaunchDockAction::Focus(*window);
        }
    }
    LaunchDockAction::Launch(pin.clone())
}

/// Per-pin running lamps for the current `(WM_CLASS class, window)`
/// pairs — same matching rule as [`resolve_click`], so the lamp and
/// the click can never disagree about whether an app counts as
/// running.
fn running_lamps<W>(pins: &[AppEntry], running: &[(String, W)]) -> Vec<bool> {
    pins.iter()
        .map(|pin| running.iter().any(|(class, _)| match_window_class(std::slice::from_ref(pin), class).is_some()))
        .collect()
}

/// Moves `items[from]` so it ends up in slot `to` (remove + insert:
/// the tiles between the two slots shift by one, they don't swap).
fn move_pin<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from >= items.len() || from == to {
        return;
    }
    let item = items.remove(from);
    items.insert(to.min(items.len()), item);
}

/// Darkens one tile's band of the assembled strip buffer — the
/// picked-up ghost. Operates on the raw bytes (tiles are opaque, so
/// premultiplied and straight RGBA agree, same reasoning as
/// `paint::draw_text`'s).
fn ghost_slot(pixels: &mut [u8], tile: u32, slot: usize) {
    let slot_bytes = (tile * tile * 4) as usize;
    let start = slot * slot_bytes;
    let Some(region) = pixels.get_mut(start..start + slot_bytes) else { return };
    for pixel in region.as_chunks_mut::<4>().0 {
        pixel[0] = pixel[0].saturating_sub(48);
        pixel[1] = pixel[1].saturating_sub(48);
        pixel[2] = pixel[2].saturating_sub(48);
    }
}

/// `$XDG_STATE_HOME/chonkstep/dock`, or the `~/.local/state` fallback
/// — the same resolution as `theme_select.rs`'s and `wallpaper.rs`'s
/// state files, which live right next to this one.
fn state_path() -> Option<PathBuf> {
    crate::startup::state_file("dock")
}

/// Reads the pin file and resolves each id against `apps`. Stale ids
/// (no matching `.desktop` entry this session) are dropped with a
/// warning; the file itself is left alone — the next mutation's
/// `persist` rewrites it (see the module doc for why not at load).
fn load_pins(path: &Path, apps: &[AppEntry]) -> Vec<AppEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut pins = Vec::new();
    for id in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match apps.iter().find(|app| app.id == id) {
            Some(app) => pins.push(app.clone()),
            None => tracing::warn!(id, "dropping stale launcher pin with no matching .desktop entry"),
        }
    }
    pins
}

/// Writes the pins as one desktop-file id per line — the whole
/// persistence format, human-editable on purpose like the theme and
/// wallpaper files beside it.
fn save_pins(path: &Path, pins: &[AppEntry]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    for pin in pins {
        text.push_str(&pin.id);
        text.push('\n');
    }
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::AppCategory;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn entry(id: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: id.to_string(),
            exec: vec![id.to_string()],
            terminal: false,
            category: AppCategory::Other,
            startup_wm_class: None,
        }
    }

    fn ids(pins: &[AppEntry]) -> Vec<&str> {
        pins.iter().map(|pin| pin.id.as_str()).collect()
    }

    /// A unique per-test state file under the system temp dir, so
    /// parallel tests never share a file and no environment variables
    /// need mutating (env is process-global; a test touching it would
    /// race every other test in the binary).
    fn temp_state_file(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir()
            .join(format!("chonkstep-launchdock-{}-{tag}-{unique}", std::process::id()))
            .join("dock")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn pins_round_trip_pin_unpin_and_reorder_through_the_state_file() {
        let apps = [entry("app.a"), entry("app.b"), entry("app.c")];
        let path = temp_state_file("roundtrip");

        // Pin three.
        let mut pins = apps.to_vec();
        save_pins(&path, &pins).unwrap();
        assert_eq!(ids(&load_pins(&path, &apps)), ["app.a", "app.b", "app.c"]);

        // Unpin the middle one.
        pins.remove(1);
        save_pins(&path, &pins).unwrap();
        assert_eq!(ids(&load_pins(&path, &apps)), ["app.a", "app.c"]);

        // Reorder what's left.
        move_pin(&mut pins, 0, 1);
        save_pins(&path, &pins).unwrap();
        assert_eq!(ids(&load_pins(&path, &apps)), ["app.c", "app.a"]);

        cleanup(&path);
    }

    #[test]
    fn stale_ids_are_dropped_at_load_and_the_file_left_alone() {
        let apps = [entry("app.alpha"), entry("app.beta")];
        let path = temp_state_file("stale");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let raw = "app.alpha\napp.gone\napp.beta\n";
        std::fs::write(&path, raw).unwrap();

        assert_eq!(ids(&load_pins(&path, &apps)), ["app.alpha", "app.beta"]);
        // Rewriting happens on the next mutation, not at load.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);

        cleanup(&path);
    }

    #[test]
    fn persistence_format_is_one_desktop_file_id_per_line() {
        let path = temp_state_file("format");
        save_pins(&path, &[entry("org.mozilla.firefox"), entry("org.gnome.Calculator")]).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "org.mozilla.firefox\norg.gnome.Calculator\n"
        );
        cleanup(&path);
    }

    #[test]
    fn unpinning_the_last_pin_releases_the_strips_pixels_but_keeps_its_surface() {
        use wm_core::fake_backend::FakeBackend;

        // The strip's surface outlives an empty spell on purpose (see
        // the field doc on `window`), but its raster grows with the pin
        // count — up to the screen height minus one tile — and nothing
        // can draw it while it is unmapped. The re-pin path repaints in
        // full, so releasing costs the next pin nothing.
        let theme = wm_theme::default_theme::theme_by_id("nextstep-classic").unwrap();
        let primary = Rect { pos: Point::new(0, 0), size: Size::new(1920, 1200) };
        let mut backend = FakeBackend::new();
        let mut dock: LaunchDock<FakeBackend> =
            LaunchDock::new(&mut backend, &theme, primary, 56, &[], wm_theme::FontState::new());

        // Pin two, so the strip has a surface and pixels to release.
        // Written straight into the fields rather than through a drag:
        // this test is about `sync_window`, and a synthesized drag would
        // also be testing the pointer path.
        dock.pins = vec![entry("app.a"), entry("app.b")];
        dock.lit = vec![false; dock.pins.len()];
        dock.sync_window(&mut backend, &theme);
        let window = dock.window.expect("pinning creates the strip's surface");
        assert!(dock.mapped, "a strip with pins is mapped");
        assert!(
            backend.shell_buffer_bytes.get(&window).is_some_and(|bytes| *bytes > 0),
            "the strip must hold pixels before the unpin under test"
        );

        // Unpin both — the empty-strip arm of `sync_window`.
        dock.pins.clear();
        dock.lit.clear();
        dock.sync_window(&mut backend, &theme);
        assert!(!dock.mapped, "an empty strip is unmapped");
        assert_eq!(dock.window, Some(window), "the surface must survive the empty spell");
        assert!(backend.shell_geometries.contains_key(&window), "released, not destroyed");
        assert_eq!(
            backend.shell_buffer_bytes.get(&window),
            None,
            "the unmapped strip kept its raster"
        );

        // Re-pinning repaints, so nothing was lost by releasing.
        dock.pins = vec![entry("app.c")];
        dock.lit = vec![false];
        dock.sync_window(&mut backend, &theme);
        assert_eq!(dock.window, Some(window), "re-pinning reuses the surface");
        assert!(
            backend.shell_buffer_bytes.get(&window).is_some_and(|bytes| *bytes > 0),
            "re-pinning must repaint the strip"
        );
    }

    #[test]
    fn a_press_becomes_a_drag_only_past_the_threshold() {
        let press = Point::new(20, 80);
        assert!(!crossed_threshold(press, Point::new(22, 82), 4), "sub-threshold wiggle is still a click");
        assert!(crossed_threshold(press, Point::new(24, 80), 4), "the threshold itself crosses, matching desktop.rs");
        assert!(crossed_threshold(press, Point::new(20, 70), 4), "either axis alone is enough");
    }

    #[test]
    fn slots_resolve_over_the_strip_extent_and_the_empty_strips_first_slot() {
        let primary = Rect { pos: Point::new(0, 0), size: Size::new(1920, 1200) };
        let origin = strip_origin(primary, 56);

        // Three pins: y = 0..168 on the left edge, one slot per tile.
        // The strip now starts at the corner itself, the Clip having
        // moved to the bottom-right, so the first tile is slot 0.
        assert_eq!(slot_at(origin, 56, 3, Point::new(10, 4)), Some(0));
        assert_eq!(slot_at(origin, 56, 3, Point::new(55, 167)), Some(2));
        assert_eq!(slot_at(origin, 56, 3, Point::new(10, 168)), None, "below the last tile");
        assert_eq!(slot_at(origin, 56, 3, Point::new(56, 4)), None, "right of the strip");
        assert_eq!(slot_at(origin, 56, 3, Point::new(10, -1)), None, "above the strip is not the strip");

        // The empty strip's pin zone is its would-be first slot: the
        // caller clamps a zero pin count up to one before asking, so
        // one is the number that actually reaches `slot_at`.
        assert_eq!(slot_at(origin, 56, 1, Point::new(10, 4)), Some(0));
        assert_eq!(slot_at(origin, 56, 1, Point::new(10, 60)), None);
    }

    /// The strip follows the primary monitor rather than the desktop
    /// origin: with a second head to the left, the desktop's origin is
    /// on that other screen, and a root-anchored strip would sit there
    /// alone while the Clip stayed on the primary.
    /// Regression test for the strip staying behind when the monitor
    /// arrangement changes.
    ///
    /// The strip anchors to the primary monitor, but it is not owned
    /// by `Desktop`, so nothing repositioned it when a display was
    /// plugged in or the session resized: the Clip moved to the new
    /// primary and the strip stayed on the old one, hit-testing clicks
    /// at coordinates it no longer occupied. Found in review, and
    /// invisible to the tests of the day because nothing here could
    /// build a `LaunchDock` at all - which is why `wm-core` now
    /// exposes its `Backend` double behind `test-support`.
    /// The strip's tile edge is derived from the UI scale by the shell,
    /// not by the strip, so a live rescale reaches it as a new `tile`.
    /// It must land on exactly what a strip built at that tile would
    /// hold — including the drag threshold, which is derived from the
    /// tile a second time and so is the field most likely to be left
    /// behind.
    #[test]
    fn restyling_to_a_new_tile_matches_a_strip_built_at_that_tile() {
        use wm_core::fake_backend::FakeBackend;

        let theme = wm_theme::default_theme::nextstep_classic();
        let mut backend = FakeBackend::new();
        let primary = Rect { pos: Point::new(0, 0), size: Size::new(640, 480) };

        let mut restyled: LaunchDock<FakeBackend> = LaunchDock::new(&mut backend, &theme, primary, 56, &[], wm_theme::FontState::new());
        restyled.restyle(&mut backend, &theme, 112);

        // Compared against the shared derivation rather than against a
        // second strip built at 112: standing one up costs a fontconfig
        // scan, and the divergence a native comparison used to guard
        // against — two copies of this formula drifting apart — is what
        // hoisting `drag_threshold_for_tile` structurally removed.
        assert_eq!(restyled.tile, 112);
        assert_eq!(restyled.drag_threshold, drag_threshold_for_tile(112));

        // And it got there with the primary rect held still.
        // `reposition` early-returns when the primary is unchanged,
        // which is exactly the case during a restyle: if `restyle` were
        // ever implemented by delegating to it, every scale change that
        // did not coincide with a monitor being plugged in would be
        // silently dropped.
        assert_eq!(restyled.primary, primary, "a restyle must not depend on the monitor having moved");
    }

    #[test]
    fn repositioning_moves_the_strip_onto_the_new_primary_monitor() {
        use wm_core::fake_backend::FakeBackend;

        let theme = wm_theme::default_theme::nextstep_classic();
        let mut backend = FakeBackend::new();
        let start = Rect { pos: Point::new(0, 0), size: Size::new(1920, 1200) };
        let mut dock: LaunchDock<FakeBackend> = LaunchDock::new(&mut backend, &theme, start, 56, &[], wm_theme::FontState::new());
        assert_eq!(strip_origin(dock.primary, 56), Point::new(0, 0));

        // A second display arrives to the left, so the primary's
        // origin moves right.
        let moved = Rect { pos: Point::new(1600, 0), size: Size::new(1920, 1200) };
        dock.reposition(&mut backend, &theme, moved);

        assert_eq!(dock.primary, moved, "the strip must adopt the new primary");
        assert_eq!(
            strip_origin(dock.primary, 56),
            Point::new(1600, 0),
            "and sit in that monitor's corner, not on the old one"
        );

        // Repositioning to where it already is changes nothing.
        dock.reposition(&mut backend, &theme, moved);
        assert_eq!(dock.primary, moved);
    }

    #[test]
    fn the_strip_anchors_to_the_primary_monitor_not_the_desktop_origin() {
        let primary = Rect { pos: Point::new(1600, 0), size: Size::new(1920, 1200) };
        let origin = strip_origin(primary, 56);
        assert_eq!(origin, Point::new(1600, 0), "the primary's own corner, which the Clip has vacated");

        assert_eq!(slot_at(origin, 56, 3, Point::new(1610, 4)), Some(0), "hit-testing follows the strip");
        assert_eq!(slot_at(origin, 56, 3, Point::new(10, 4)), None, "the other monitor is not the strip");
    }

    #[test]
    fn move_pin_shifts_neighbors_rather_than_swapping() {
        let mut items = vec!["a", "b", "c", "d"];
        move_pin(&mut items, 0, 2);
        assert_eq!(items, ["b", "c", "a", "d"]);
        move_pin(&mut items, 3, 0);
        assert_eq!(items, ["d", "b", "c", "a"]);
        move_pin(&mut items, 1, 9);
        assert_eq!(items, ["d", "c", "a", "b"], "an overshot target clamps to the end");
    }

    #[test]
    fn clicks_focus_a_matching_running_window_and_launch_otherwise() {
        let mut pin = entry("org.mozilla.firefox");
        pin.name = "Firefox".to_string();
        pin.startup_wm_class = Some("Navigator".to_string());

        // StartupWMClass match: the strongest signal, per the
        // match_window_class contract — Focus names the actual window.
        let running = [("xterm".to_string(), 11u32), ("Navigator".to_string(), 42u32)];
        assert!(matches!(resolve_click(&pin, &running), LaunchDockAction::Focus(42)));

        // Name match, case-insensitive.
        let by_name = [("firefox".to_string(), 7u32)];
        assert!(matches!(resolve_click(&pin, &by_name), LaunchDockAction::Focus(7)));

        // Nothing running that matches: launch a clone of the entry.
        let unrelated = [("xterm".to_string(), 11u32)];
        assert!(matches!(resolve_click(&pin, &unrelated), LaunchDockAction::Launch(app) if app == pin));
    }

    #[test]
    fn running_lamps_light_exactly_the_matched_pins() {
        let mut firefox = entry("org.mozilla.firefox");
        firefox.startup_wm_class = Some("Navigator".to_string());
        let calculator = entry("org.gnome.Calculator");
        let pins = [firefox, calculator];

        let running = [("Navigator".to_string(), 42u32)];
        assert_eq!(running_lamps(&pins, &running), [true, false]);
        // The empty slice pins no window-id type, so name one — any
        // id type gives the same lamps, which is the point of the
        // helper being generic.
        assert_eq!(running_lamps::<u32>(&pins, &[]), [false, false]);
    }

    #[test]
    fn a_pin_change_invalidates_lamps_even_when_the_client_set_did_not_change() {
        use wm_core::fake_backend::FakeBackend;

        let theme = wm_theme::default_theme::nextstep_classic();
        let mut backend = FakeBackend::new();
        let primary = Rect { pos: Point::new(0, 0), size: Size::new(640, 480) };
        let mut dock: LaunchDock<FakeBackend> =
            LaunchDock::new(&mut backend, &theme, primary, 56, &[], wm_theme::FontState::new());
        // This test owns no real session state. Pinning below should
        // exercise reconciliation, not write the user's launcher file.
        dock.state_path = None;

        assert!(dock.running_refresh_needed(), "startup must reconcile persisted pins once");
        dock.update_running(&mut backend, &theme, &[]);
        assert!(!dock.running_refresh_needed());

        assert!(dock.try_pin_at(&mut backend, &theme, Point::new(10, 10), &entry("org.example.App")));
        assert!(
            dock.running_refresh_needed(),
            "the unchanged empty client cache cannot be allowed to leave a newly pinned app provisional"
        );
    }
}
