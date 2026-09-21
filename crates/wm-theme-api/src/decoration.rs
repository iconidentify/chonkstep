use crate::{Point, Rect, Size};

/// The frame's geometry/glyph recipe, independent of palette and appearance.
/// A name can be reserved here before its renderer is available in a build.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecorationStyle {
    /// Configuration policy: resolve against the theme before rendering.
    Auto,
    /// Chiseled WindowMaker/NeXTSTEP chrome, preserving existing defaults.
    #[default]
    WindowMaker,
    /// Classic System 7.5 document-window chrome.
    System7,
    /// BeOS R5 tabbed window chrome.
    BeOS,
    /// Modern, token-driven chrome shared by present and future themes.
    Modern,
}

impl DecorationStyle {
    /// Parse the stable configuration spelling; unknown names are not aliases.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "auto" => Some(Self::Auto),
            "windowmaker" => Some(Self::WindowMaker),
            "system7" => Some(Self::System7),
            "beos" => Some(Self::BeOS),
            "modern" => Some(Self::Modern),
            _ => None,
        }
    }

    /// Stable configuration and diagnostic spelling.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::WindowMaker => "windowmaker",
            Self::System7 => "system7",
            Self::BeOS => "beos",
            Self::Modern => "modern",
        }
    }
}

/// A titlebar button. The classic NeXTSTEP desktop has no maximize
/// button at all (zoom is menu/keybinding-driven); this deliberately
/// breaks from the classic recipe on that one point by adding one,
/// since a directly-clickable maximize is expected UI on a modern
/// desktop.
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
    /// Fully transparent input-only space surrounding the visible frame on
    /// all four sides. Included in frame_size and client_offset; excluded
    /// from placement, snapping, previews and opaque regions.
    pub input_margin: u32,
    /// Transparent space beside a short title tab. Both backends must let
    /// pointer input pass through this frame-local rectangle.
    pub input_exclusion: Option<Rect>,
    pub client_offset: Point,
    pub titlebar_height: u32,
    pub button_hitboxes: Vec<(ButtonKind, Rect)>,
    pub resize_hitboxes: Vec<(ResizeEdge, Rect)>,
    /// The frame's height when "shaded" (the classic roll-up-to-
    /// titlebar state) — just enough for the titlebar plus top/bottom
    /// border, none of the content. A theme-owned value (only the theme
    /// knows its exact border/bevel widths) rather than something
    /// `wm-core` derives from the other fields, so it stays correct
    /// under any future border styling.
    pub shaded_frame_height: u32,
}

impl DecorationLayout {
    /// Visible frame bounds in frame-local coordinates, also for a shortened
    /// shaded layout. Computing this avoids stale cached bounds after resize.
    pub fn visual_bounds(&self) -> Rect {
        let margin = self.input_margin.min(self.frame_size.w / 2).min(self.frame_size.h / 2);
        Rect::new(Point::new(margin as i32, margin as i32),
            Size::new(self.frame_size.w - margin * 2, self.frame_size.h - margin * 2))
    }

    /// This layout with its titlebar band taken out: the frame shortened by
    /// `titlebar_height`, the client moved up into the space, no buttons, and
    /// every resize hitbox squeezed across the removed band so the handles
    /// above and below it still meet.
    ///
    /// The generic answer to [`ThemeEngine::edges_layout_at`], exact for any
    /// recipe whose titlebar band ends where the client begins and does not
    /// include the frame's top border. The built-in recipes do not all draw
    /// their titlebar that way and implement edge frames themselves.
    pub fn without_titlebar(&self) -> DecorationLayout {
        let bottom = self.client_offset.y;
        let strip = self.titlebar_height.min(bottom.max(0) as u32) as i32;
        let top = bottom - strip;
        let squeeze = |y: i32| if y <= top { y } else if y >= bottom { y - strip } else { top };
        let resize_hitboxes = self.resize_hitboxes.iter().filter_map(|&(edge, rect)| {
            let (y0, y1) = (squeeze(rect.pos.y), squeeze(rect.pos.y + rect.size.h as i32));
            (y1 > y0).then(|| (edge, Rect::new(Point::new(rect.pos.x, y0), Size::new(rect.size.w, (y1 - y0) as u32))))
        }).collect();
        let frame_size = Size::new(self.frame_size.w, self.frame_size.h - strip as u32);
        DecorationLayout {
            frame_size,
            input_margin: self.input_margin,
            input_exclusion: None,
            client_offset: Point::new(self.client_offset.x, top),
            titlebar_height: 0,
            button_hitboxes: Vec::new(),
            resize_hitboxes,
            // No titlebar to roll up into: an edge frame cannot be shaded.
            shaded_frame_height: frame_size.h,
        }
    }
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

/// One independently retained piece of window chrome. Decorations are
/// deliberately sparse: the client owns the large interior of a frame, so
/// retaining that interior in a theme buffer wastes memory and turns a tiny
/// titlebar change into full-window damage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationPart {
    pub offset: Point,
    pub buffer: DecorationBuffer,
}

/// An opaque, uniformly colored chrome rectangle. Backends retain geometry
/// and color, rather than allocating or uploading a rectangle of identical
/// RGBA pixels. Rectangles and raster parts must be disjoint and frame-local.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationSolid {
    pub rect: Rect,
    pub rgb: [u8; 3],
}

/// Rasterized chrome for one frame. `frame_size` describes the complete frame
/// for placement and gap filling; `parts` contain only visible chrome bands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecorationSurface {
    pub frame_size: Size,
    pub parts: Vec<DecorationPart>,
    /// Empty for legacy raster recipes: no extra heap allocation or change to
    /// the allocation size of their independently retained raster parts.
    pub solids: Vec<DecorationSolid>,
    /// Optional effects contain no pixels and never change the input margin.
    pub shadow: Option<crate::DecorationShadow>,
    pub shape: Option<crate::DecorationShape>,
}

impl DecorationSurface {
    /// Compatibility adapter for simple themes that still paint one rectangle.
    pub fn full(buffer: DecorationBuffer) -> Self {
        Self {
            frame_size: Size::new(buffer.width, buffer.height),
            parts: vec![DecorationPart { offset: Point::new(0, 0), buffer }],
            solids: Vec::new(),
            shadow: None,
            shape: None,
        }
    }

    /// Exact retained CPU pixel storage, useful for diagnostics and regression
    /// tests which assert that cost follows the chrome perimeter.
    pub fn retained_bytes(&self) -> usize {
        self.parts.iter().map(|part| part.buffer.pixels.len()).sum()
    }
}

/// The boundary `wm-core` depends on. Implemented by `wm-theme`, which
/// owns the theme data model and rendering stack — `wm-core` never sees
/// a `Theme`, a color, or a font, only this trait's inputs/outputs.
pub trait ThemeEngine {
    /// Cheap, no rasterization — safe to call on every state change.
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout;

    /// Scale-aware layout hook. Existing themes remain valid at scale 1;
    /// engines which cache per-output variants override this method.
    fn layout_at(&self, request: &DecorationRequest, scale: f32) -> DecorationLayout {
        let _ = scale;
        self.layout(request)
    }

    /// Rasterizes the decoration. Callers should only invoke this when
    /// `request` actually changed since the last render.
    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer;

    /// Sparse decoration output. The default preserves source compatibility;
    /// real raster engines should emit only the chrome bands.
    fn render_surface(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationSurface {
        DecorationSurface::full(self.render(request, layout))
    }

    /// Scale-aware sparse render companion to [`Self::layout_at`].
    fn render_surface_at(
        &self,
        request: &DecorationRequest,
        layout: &DecorationLayout,
        scale: f32,
    ) -> DecorationSurface {
        let _ = scale;
        self.render_surface(request, layout)
    }

    /// Layout for an *edge frame*: this theme's borders and resize hitboxes
    /// around a client that draws its own titlebar, with a `titlebar_height`
    /// of zero and no buttons. The window still has exactly one titlebar, the
    /// client's, and the frame still resizes.
    ///
    /// The default removes the titlebar band from [`Self::layout_at`]; see
    /// [`DecorationLayout::without_titlebar`] for when that is exact.
    fn edges_layout_at(&self, request: &DecorationRequest, scale: f32) -> DecorationLayout {
        self.layout_at(request, scale).without_titlebar()
    }

    /// Paints an [`Self::edges_layout_at`] layout: borders only, no title and
    /// no buttons. The default hands the layout to the ordinary renderer,
    /// which is correct for a theme that paints from the layout it is given.
    fn render_edges_at(&self, request: &DecorationRequest, layout: &DecorationLayout, scale: f32) -> DecorationSurface {
        self.render_surface_at(request, layout, scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A titlebar band below a one-pixel top border, with a corner arm
    /// reaching down into the band and a side handle running through it.
    #[test]
    fn removing_the_titlebar_closes_the_band_and_keeps_every_handle_joined() {
        let full = DecorationLayout {
            frame_size: Size::new(120, 122),
            input_margin: 0,
            input_exclusion: None,
            client_offset: Point::new(1, 21),
            titlebar_height: 20,
            button_hitboxes: vec![(ButtonKind::Close, Rect::new(Point::new(4, 4), Size::new(14, 14)))],
            resize_hitboxes: vec![
                (ResizeEdge::NorthWest, Rect::new(Point::new(0, 0), Size::new(1, 10))),
                (ResizeEdge::North, Rect::new(Point::new(10, 0), Size::new(100, 1))),
                (ResizeEdge::West, Rect::new(Point::new(0, 10), Size::new(1, 102))),
                (ResizeEdge::SouthEast, Rect::new(Point::new(110, 112), Size::new(10, 10))),
            ],
            shaded_frame_height: 22,
        };
        let edges = full.without_titlebar();
        assert_eq!(edges.frame_size, Size::new(120, 102));
        assert_eq!(edges.client_offset, Point::new(1, 1), "the client moves up into the band");
        assert_eq!(edges.titlebar_height, 0);
        assert!(edges.button_hitboxes.is_empty());
        assert_eq!(edges.shaded_frame_height, 102, "there is nothing to shade into");
        assert_eq!(
            edges.resize_hitboxes,
            vec![
                (ResizeEdge::NorthWest, Rect::new(Point::new(0, 0), Size::new(1, 1))),
                (ResizeEdge::North, Rect::new(Point::new(10, 0), Size::new(100, 1))),
                (ResizeEdge::West, Rect::new(Point::new(0, 1), Size::new(1, 91))),
                (ResizeEdge::SouthEast, Rect::new(Point::new(110, 92), Size::new(10, 10))),
            ]
        );
    }
}
