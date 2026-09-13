//! Explicit dispatch for every window-derived shell surface. The shell calls
//! this API without knowing the active recipe or any named theme.
use crate::{menu, overview, switcher, FontState, Theme};
use wm_theme_api::{DecorationBuffer, DecorationStyle, Size};

#[derive(Clone)]
pub struct UiChrome {
    legacy: crate::styles::system7::ui::UiChrome,
    modern: Option<crate::modern_ui::ModernUi>,
    style: DecorationStyle,
}

impl UiChrome {
    pub fn new(theme: &Theme, fonts: FontState, style: DecorationStyle, scale: f32) -> Self {
        let style = theme.resolve_style(style);
        if style == DecorationStyle::Modern || theme.chrome.is_some() {
            fonts.prepare_modern();
        }
        Self {
            legacy: crate::styles::system7::ui::UiChrome::new(theme, fonts.clone(), style, scale),
            modern: (style == DecorationStyle::Modern)
                .then(|| crate::modern_ui::ModernUi::new(theme, fonts, scale)),
            style,
        }
    }
    pub fn style(&self) -> DecorationStyle {
        self.style
    }
    pub fn overview_ink(&self) -> Option<([u8; 3], u32)> {
        self.modern
            .as_ref()
            .map_or_else(|| self.legacy.overview_ink(), |ui| Some(ui.overview_ink()))
    }
    #[allow(clippy::too_many_arguments)]
    pub fn menu(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        title: &str,
        items: &[menu::MenuItem],
        highlighted: Option<usize>,
        closable: bool,
    ) -> menu::MenuRender {
        match &self.modern {
            Some(ui) => ui.menu(
                theme,
                &mut ui.fonts.modern_system(),
                title,
                items,
                highlighted,
                closable,
            ),
            None => self
                .legacy
                .menu(theme, fonts, title, items, highlighted, closable),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn icon(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        size: u32,
        title: &str,
        preview: Option<&DecorationBuffer>,
    ) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.icon(
                theme,
                &mut ui.fonts.modern_system(),
                &mut ui.fonts.modern_swash(),
                size,
                title,
                preview,
            ),
            None => self.legacy.icon(theme, fonts, cache, size, title, preview),
        }
    }
    pub fn switcher(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entries: &[switcher::SwitcherEntry],
        selected: usize,
        tile: u32,
    ) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.switcher(
                theme,
                &mut ui.fonts.modern_system(),
                &mut ui.fonts.modern_swash(),
                entries,
                selected,
                tile,
            ),
            None => self
                .legacy
                .switcher(theme, fonts, cache, entries, selected, tile),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn label(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        text: &str,
        width: u32,
        height: u32,
        inverted: bool,
    ) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.label(
                theme,
                &mut ui.fonts.modern_system(),
                &mut ui.fonts.modern_swash(),
                text,
                width,
                height,
                inverted,
            ),
            None => self
                .legacy
                .label(theme, fonts, cache, text, width, height, inverted),
        }
    }
    pub fn overview(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entries: &[overview::OverviewEntry<'_>],
        workspace: (usize, usize),
        layout: &overview::OverviewLayout,
    ) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.overview(
                theme,
                &mut ui.fonts.modern_system(),
                &mut ui.fonts.modern_swash(),
                entries,
                workspace,
                layout,
            ),
            None => self
                .legacy
                .overview(theme, fonts, cache, entries, workspace, layout),
        }
    }
    pub fn workspace_close(&self, edge: u32) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.workspace_close(edge),
            None => self.legacy.workspace_close(edge),
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn selection(
        &self,
        theme: &Theme,
        fonts: &mut cosmic_text::FontSystem,
        cache: &mut cosmic_text::SwashCache,
        entry: &overview::OverviewEntry<'_>,
        cell: Size,
        pad: u32,
    ) -> DecorationBuffer {
        match &self.modern {
            Some(ui) => ui.selection(
                theme,
                &mut ui.fonts.modern_system(),
                &mut ui.fonts.modern_swash(),
                entry,
                cell,
                pad,
            ),
            None => self.legacy.selection(theme, fonts, cache, entry, cell, pad),
        }
    }
}
