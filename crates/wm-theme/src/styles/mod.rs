//! Frame recipes. Font discovery, title/glyph caches and scale variants live
//! in the engine; each recipe owns only its geometry and sparse painting.
pub(crate) mod windowmaker;

use wm_theme_api::DecorationStyle;

/// Renderers actually implemented in this build. Tests/benchmarks iterate this
/// list; reserved names must never silently fall back to another style's pixels.
pub const SUPPORTED_DECORATION_STYLES: &[DecorationStyle] = &[DecorationStyle::WindowMaker];

#[derive(Clone, Copy)]
pub(crate) enum FrameStyle {
    WindowMaker,
}

impl FrameStyle {
    pub(crate) const fn name(self) -> DecorationStyle {
        match self { Self::WindowMaker => DecorationStyle::WindowMaker }
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
            DecorationStyle::System7 => Err(UnsupportedDecorationStyle(style)),
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
        for style in [DecorationStyle::WindowMaker, DecorationStyle::System7] {
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
        assert!(RasterThemeEngine::with_fonts(theme, fonts).with_style(DecorationStyle::System7).is_err(),
            "a reserved renderer must not silently draw WindowMaker");
    }
}
