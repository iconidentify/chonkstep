//! Frame recipes. Font discovery, title/glyph caches and scale variants live
//! in the engine; each recipe owns only its geometry and sparse painting.
pub(crate) mod windowmaker;
pub(crate) mod system7;
pub(crate) mod modern;
pub(crate) mod beos;

use wm_theme_api::{DecorationStyle, Point, Rect, ResizeEdge, Size};

/// The resize handles of an edge frame, which has no titlebar to hang
/// handles on: an L-shaped corner reaching `corner` along both sides, and
/// a straight band between the corners on every edge, each exactly as
/// thick as the chrome on its side. Corners come first, so they win where
/// they meet a band.
pub(crate) fn edge_ring(frame: Size, top: u32, right: u32, bottom: u32, left: u32, corner: u32) -> Vec<(ResizeEdge, Rect)> {
    let (w, h) = (frame.w, frame.h);
    let (corner_w, corner_h) = (corner.min(w / 2), corner.min(h / 2));
    let (top, bottom) = (top.min(corner_h), bottom.min(corner_h));
    let (left, right) = (left.min(corner_w), right.min(corner_w));
    [
        (ResizeEdge::NorthWest, 0, 0, corner_w, top),
        (ResizeEdge::NorthWest, 0, 0, left, corner_h),
        (ResizeEdge::NorthEast, w - corner_w, 0, corner_w, top),
        (ResizeEdge::NorthEast, w - right, 0, right, corner_h),
        (ResizeEdge::SouthWest, 0, h - bottom, corner_w, bottom),
        (ResizeEdge::SouthWest, 0, h - corner_h, left, corner_h),
        (ResizeEdge::SouthEast, w - corner_w, h - bottom, corner_w, bottom),
        (ResizeEdge::SouthEast, w - right, h - corner_h, right, corner_h),
        (ResizeEdge::North, corner_w, 0, w - corner_w * 2, top),
        (ResizeEdge::South, corner_w, h - bottom, w - corner_w * 2, bottom),
        (ResizeEdge::West, 0, corner_h, left, h - corner_h * 2),
        (ResizeEdge::East, w - right, corner_h, right, h - corner_h * 2),
    ]
    .into_iter()
    .filter(|&(_, _, _, rw, rh)| rw > 0 && rh > 0)
    .map(|(edge, x, y, rw, rh)| (edge, Rect::new(Point::new(x as i32, y as i32), Size::new(rw, rh))))
    .collect()
}

/// Renderers actually implemented in this build. Tests/benchmarks iterate this
/// list; reserved names must never silently fall back to another style's pixels.
pub const SUPPORTED_DECORATION_STYLES: &[DecorationStyle] = &[DecorationStyle::WindowMaker, DecorationStyle::System7, DecorationStyle::BeOS, DecorationStyle::Modern];

#[derive(Clone, Copy)]
pub(crate) enum FrameStyle {
    WindowMaker,
    System7,
    BeOS,
    Modern,
}

impl FrameStyle {
    pub(crate) const fn name(self) -> DecorationStyle {
        match self { Self::WindowMaker => DecorationStyle::WindowMaker, Self::System7 => DecorationStyle::System7, Self::BeOS => DecorationStyle::BeOS, Self::Modern => DecorationStyle::Modern }
    }
}

/// A recognized style whose actual renderer is not present in this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedDecorationStyle(pub DecorationStyle);

impl std::fmt::Display for UnsupportedDecorationStyle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "decoration style {:?} is not implemented in this build", self.0.name())
    }
}

impl std::error::Error for UnsupportedDecorationStyle {}

impl TryFrom<DecorationStyle> for FrameStyle {
    type Error = UnsupportedDecorationStyle;

    fn try_from(style: DecorationStyle) -> Result<Self, Self::Error> {
        match style {
            DecorationStyle::WindowMaker => Ok(Self::WindowMaker),
            DecorationStyle::System7 => Ok(Self::System7),
            DecorationStyle::BeOS => Ok(Self::BeOS),
            DecorationStyle::Modern => Ok(Self::Modern),
            DecorationStyle::Auto => Err(UnsupportedDecorationStyle(style)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FontState, RasterThemeEngine};
    use wm_theme_api::{DecorationRequest, Size, ThemeEngine};

    #[test]
    fn stable_names_roundtrip_and_unknown_names_are_rejected() {
        #[derive(serde::Serialize, serde::Deserialize)]
        struct Config { style: DecorationStyle }
        assert_eq!(DecorationStyle::default(), DecorationStyle::WindowMaker);
        for style in [DecorationStyle::WindowMaker, DecorationStyle::System7, DecorationStyle::BeOS] {
            assert_eq!(DecorationStyle::from_name(style.name()), Some(style));
            let encoded = toml::to_string(&Config { style }).unwrap();
            assert_eq!(toml::from_str::<Config>(&encoded).unwrap().style, style);
        }
        for name in ["", "System7", "system7.5", "nextstep", "invalid"] {
            assert_eq!(DecorationStyle::from_name(name), None);
            assert!(toml::from_str::<Config>(&format!("style = {name:?}")).is_err());
        }
    }

    #[test]
    fn existing_constructors_and_explicit_windowmaker_have_identical_outputs() {
        let theme = crate::default_theme::nextstep_classic();
        let fonts = FontState::new();
        let default = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone());
        let explicit = RasterThemeEngine::with_fonts_at_scale(theme.clone(), fonts.clone(), 1.0)
            .with_style(DecorationStyle::WindowMaker).unwrap();
        assert_eq!(default.style(), DecorationStyle::WindowMaker);
        assert_eq!(explicit.style(), DecorationStyle::WindowMaker);
        let request = DecorationRequest { content_size: Size::new(800, 600), title: "Explicit style".into(),
            focused: true, resizable: true, buttons: Vec::new() };
        for scale in [1.0, 1.5, 2.0] {
            let layout = default.layout_at(&request, scale);
            assert_eq!(explicit.layout_at(&request, scale), layout);
            assert_eq!(default.render_surface_at(&request, &layout, scale), explicit.render_surface_at(&request, &layout, scale));
        }
        let system7 = RasterThemeEngine::with_fonts(theme, fonts).with_style(DecorationStyle::System7).unwrap();
        assert_eq!(system7.style(), DecorationStyle::System7);
        assert_ne!(system7.layout(&request), default.layout(&request));
    }

    /// Every recipe draws an edge frame from its own metrics: no titlebar
    /// and no buttons, a top edge that is a border like the other three,
    /// a handle on every edge and corner, and not one handle or painted
    /// pixel over the client, whose own header bar lives there.
    #[test]
    fn every_recipe_draws_an_edge_frame_with_borders_and_handles_but_no_titlebar() {
        use wm_theme_api::{Rect, ResizeEdge};
        let overlaps = |a: Rect, b: Rect| {
            a.pos.x < b.pos.x + b.size.w as i32 && b.pos.x < a.pos.x + a.size.w as i32
                && a.pos.y < b.pos.y + b.size.h as i32 && b.pos.y < a.pos.y + a.size.h as i32
        };
        let fonts = FontState::new();
        let request = DecorationRequest { content_size: Size::new(640, 480), title: "Edge frame".into(),
            focused: true, resizable: true, buttons: Vec::new() };
        for &style in SUPPORTED_DECORATION_STYLES {
            let theme = if style == DecorationStyle::Modern {
                crate::modern::theme("relay", crate::Appearance::Dark).unwrap()
            } else {
                crate::default_theme::nextstep_classic()
            };
            let engine = RasterThemeEngine::with_fonts(theme, fonts.clone()).with_style(style).unwrap();
            let name = style.name();
            for scale in [1.0, 1.5, 2.0] {
                let full = engine.layout_at(&request, scale);
                let edges = engine.edges_layout_at(&request, scale);
                let visual = edges.visual_bounds();
                assert_eq!(edges.titlebar_height, 0, "{name} at {scale}");
                assert!(edges.button_hitboxes.is_empty(), "{name} at {scale}");
                assert_eq!(visual.size.w, full.visual_bounds().size.w, "{name} at {scale}: the sides are the full frame's");
                assert!(visual.size.h < full.visual_bounds().size.h, "{name} at {scale}: the titlebar is gone");
                assert_eq!(
                    edges.client_offset.y - visual.pos.y,
                    edges.client_offset.x - visual.pos.x,
                    "{name} at {scale}: the top edge is a border like the left one"
                );
                assert_eq!(edges.shaded_frame_height, edges.frame_size.h, "{name} at {scale}");
                let frame = Rect::new(Point::new(0, 0), edges.frame_size);
                let client = Rect::new(edges.client_offset, request.content_size);
                for edge in [ResizeEdge::North, ResizeEdge::South, ResizeEdge::East, ResizeEdge::West,
                    ResizeEdge::NorthEast, ResizeEdge::NorthWest, ResizeEdge::SouthEast, ResizeEdge::SouthWest] {
                    assert!(edges.resize_hitboxes.iter().any(|(e, _)| *e == edge), "{name} at {scale}: no {edge:?} handle");
                }
                for &(edge, rect) in &edges.resize_hitboxes {
                    assert!(!overlaps(rect, client), "{name} at {scale}: the {edge:?} handle covers the client");
                    assert_eq!(rect.intersection(frame), Some(rect), "{name} at {scale}: the {edge:?} handle leaves the frame");
                }
                let surface = engine.render_edges_at(&request, &edges, scale);
                assert_eq!(surface.frame_size, edges.frame_size, "{name} at {scale}");
                let painted: Vec<Rect> = surface.parts.iter()
                    .map(|part| Rect::new(part.offset, Size::new(part.buffer.width, part.buffer.height)))
                    .chain(surface.solids.iter().map(|solid| solid.rect))
                    .collect();
                assert!(!painted.is_empty(), "{name} at {scale}: an edge frame paints its borders");
                for rect in painted {
                    assert!(!overlaps(rect, client), "{name} at {scale}: {rect:?} paints over the client");
                    assert_eq!(rect.intersection(frame), Some(rect), "{name} at {scale}: {rect:?} leaves the frame");
                }
            }
        }
    }
}
