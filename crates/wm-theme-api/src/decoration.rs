use crate::{Point, Rect, Size};

/// A titlebar button. Classic WindowMaker has no maximize button at all
/// (zoom is menu/keybinding-driven); this deliberately breaks from that
/// one point of parity by adding one, since a directly-clickable
/// maximize is expected UI on a modern desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ButtonKind {
    Close,
    Miniaturize,
    Maximize,
}

/// Which resize edge/corner a pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonRuntimeState {
    pub kind: ButtonKind,
    pub hovered: bool,
    pub pressed: bool,
}

/// Everything a `ThemeEngine` needs to lay out and paint one window's
/// decoration. `wm-core` builds this from `Client` state whenever
/// something visible changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationRequest {
    pub content_size: Size,
    pub title: String,
    pub focused: bool,
    pub resizable: bool,
    pub buttons: Vec<ButtonRuntimeState>,
}

/// Frame-local layout: hit-test geometry plus how big the frame needs to
/// be. Pure arithmetic, no pixels — themes own exact sizing, so this is
/// the authoritative source for both `Backend::create_decoration`'s
/// frame size and `wm-core`'s hit-testing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecorationLayout {
    pub frame_size: Size,
    pub client_offset: Point,
    pub titlebar_height: u32,
    pub button_hitboxes: Vec<(ButtonKind, Rect)>,
    pub resize_hitboxes: Vec<(ResizeEdge, Rect)>,
    /// The frame's height when "shaded" (WindowMaker's roll-up-to-
    /// titlebar state) — just enough for the titlebar plus top/bottom
    /// border, none of the content. A theme-owned value (only the theme
    /// knows its exact border/bevel widths) rather than something
    /// `wm-core` derives from the other fields, so it stays correct
    /// under any future border styling.
    pub shaded_frame_height: u32,
}

/// Rasterized decoration pixels: RGBA8, row-major, no row padding
/// (`pixels.len() == width * height * 4`). Backend-agnostic — the X11
/// backend converts to server byte order and blits; a future Wayland
/// backend could hand this straight to a `wl_buffer`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationBuffer {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// The boundary `wm-core` depends on. Implemented by `wm-theme`, which
/// owns the theme data model and rendering stack — `wm-core` never sees
/// a `Theme`, a color, or a font, only this trait's inputs/outputs.
pub trait ThemeEngine {
    /// Cheap, no rasterization — safe to call on every state change.
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout;

    /// Rasterizes the decoration. Callers should only invoke this when
    /// `request` actually changed since the last render.
    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer;
}
