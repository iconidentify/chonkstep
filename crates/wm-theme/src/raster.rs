//! The real `wm_theme_api::ThemeEngine` implementation: `RasterThemeEngine`
//! rasterizes window decorations with `tiny-skia` (fills, gradients,
//! bevels, button glyphs) and `cosmic-text` (title text, with font
//! fallback). Pure Rust, no X11/Wayland dependency — see the crate-level
//! doc comment in `lib.rs`.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use wm_theme_api::{
    DecorationBuffer, DecorationLayout, DecorationRequest, DecorationStyle,
    DecorationSurface, ThemeEngine,
};

use crate::model::Theme;
use crate::styles::{system7, windowmaker, FrameStyle, UnsupportedDecorationStyle};
pub(crate) use windowmaker::draw_button_glyph;

/// The font machinery decoration text is shaped and rasterized with,
/// in a handle that is cheap to clone.
///
/// It exists as a separate type for one reason:
/// `cosmic_text::FontSystem::new()` scans the system's fonts through
/// fontconfig, which costs hundreds of milliseconds and must happen
/// exactly once per session. Restyling is the most routine thing a
/// user does to this desktop — every theme pick is one, and every one
/// of them used to re-exec the process — so a live retheme builds a
/// *new* [`RasterThemeEngine`] around the *same* font state
/// ([`RasterThemeEngine::with_fonts`]) rather than a new engine that
/// re-scans. That is the same argument the dockapp protocol makes for
/// `ThemeChanged` being a message rather than a relaunch, applied to
/// the window manager's own engine.
///
/// `Rc`, not `Arc`, and `RefCell`, not a lock: `ThemeEngine`'s methods
/// take `&self` while shaping needs `&mut`, which already pins an
/// engine to one thread — the window manager's, single-threaded by
/// design. Sharing the state does not widen that; it only lets two
/// engines that never coexist on separate threads hand it over.
#[derive(Clone)]
pub struct FontState {
    font_system: Rc<RefCell<cosmic_text::FontSystem>>,
    swash_cache: Rc<RefCell<GlyphCache>>,
    system7_fallback: Rc<RefCell<system7::Fallback>>,
}

/// Read-only font/raster cache sizes for explicit diagnostics, not RSS.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontCacheStatistics {
    /// Available database faces, not the count of eagerly loaded font objects.
    pub available_faces: usize,
    /// Image entries, including cached misses.
    pub image_entries: usize,
    /// Sum of image byte-vector capacities; excludes hash/scaler overhead.
    pub image_payload_bytes: usize,
    /// Outline entries, including cached misses.
    pub outline_entries: usize,
    /// Sum of cached outline command storage; excludes hash/scaler overhead.
    pub outline_payload_bytes: usize,
}

// Glyph keys include font, size and subpixel position. Keeping every
// title ever displayed at every scale made this session-long cache grow
// without a limit. These are soft watermarks, checked between render
// calls: one call can temporarily exceed them. Normal, warm rendering
// only compares entry counts; it does not walk the cache every frame.
const GLYPH_CACHE_BYTES: usize = 8 * 1024 * 1024;
const GLYPH_CACHE_ENTRIES: usize = 16 * 1024;
const GLYPH_CACHE_CHECK_STEP: usize = 128;

pub(crate) struct GlyphCache {
    pub(crate) swash: cosmic_text::SwashCache,
    next_check: usize,
    observed_entries: usize,
}

impl GlyphCache {
    pub(crate) fn new() -> Self {
        Self { swash: cosmic_text::SwashCache::new(), next_check: 1, observed_entries: 0 }
    }

    pub(crate) fn trim(&mut self) {
        let entries = self.swash.image_cache.len() + self.swash.outline_command_cache.len();
        if entries < self.observed_entries {
            // Public callers can clear Swash's maps themselves.
            self.next_check = 1;
        }
        self.observed_entries = entries;
        if entries < self.next_check {
            return;
        }
        let image_bytes: usize = self.swash.image_cache.values().flatten()
            .map(|image| image.data.capacity()).sum();
        let outline_bytes: usize = self.swash.outline_command_cache.values().flatten()
            .map(|commands| std::mem::size_of_val(commands.as_ref())).sum();
        if entries >= GLYPH_CACHE_ENTRIES || image_bytes.saturating_add(outline_bytes) >= GLYPH_CACHE_BYTES {
            // Release the hash tables too. Font discovery and Swash's
            // scaler context stay warm; only reproducible glyph data
            // is evicted. No font or fallback behavior changes.
            self.swash.image_cache = Default::default();
            self.swash.outline_command_cache = Default::default();
            self.next_check = 1;
            self.observed_entries = 0;
        } else {
            // Check small caches on every addition, so even a single
            // unusually large glyph is noticed on the next render.
            self.next_check = entries + if entries < GLYPH_CACHE_CHECK_STEP { 1 } else { GLYPH_CACHE_CHECK_STEP };
        }
    }
}

impl FontState {
    /// Loads the system font database. Expensive — call it once per
    /// session and clone the handle thereafter.
    pub fn new() -> Self {
        let mut font_system = cosmic_text::FontSystem::new();
        let system7_fallback = system7::Fallback::prepare(&mut font_system);
        Self {
            font_system: Rc::new(RefCell::new(font_system)),
            swash_cache: Rc::new(RefCell::new(GlyphCache::new())),
            system7_fallback: Rc::new(RefCell::new(system7_fallback)),
        }
    }

    /// The shared `FontSystem`, mutably. Shaping and measuring need
    /// `&mut`; the `RefMut` is the loan. Callers keep the loan short
    /// and never hold it across a call that could re-enter this state
    /// (the single-threaded discipline the type's doc describes) —
    /// in practice each borrow lives for exactly one render call.
    ///
    /// Public so the *shell's* text (dock tiles, icon labels, menus,
    /// the switcher) rasterizes out of the same one-per-session
    /// database as the decoration engine. Before this existed, the
    /// desktop and the launcher strip each ran their own
    /// `FontSystem::new()` scan — three font databases in one process
    /// for a type whose whole reason to exist is "exactly once per
    /// session".
    pub fn system(&self) -> std::cell::RefMut<'_, cosmic_text::FontSystem> {
        self.font_system.borrow_mut()
    }

    /// The shared glyph raster cache, mutably — [`FontState::system`]'s
    /// companion, under the same short-loan discipline. Evicts glyph
    /// images at soft 8 MiB / 16K-entry watermarks between render calls,
    /// preserving the font database and scaler context. Repeated large
    /// working sets may rasterize again instead of remaining resident.
    pub fn swash(&self) -> std::cell::RefMut<'_, cosmic_text::SwashCache> {
        let mut cache = self.swash_cache.borrow_mut();
        cache.trim();
        std::cell::RefMut::map(cache, |cache| &mut cache.swash)
    }

    /// Inspect without eviction or allocation. This walks cache entries;
    /// callers must not put it on an ordinary frame/input path.
    pub fn cache_statistics(&self) -> FontCacheStatistics {
        let cache = self.swash_cache.borrow();
        let fallback = self.system7_fallback.borrow();
        let caches = [&*cache, fallback.cache()];
        FontCacheStatistics {
            available_faces: self.font_system.borrow().db().faces().count(),
            image_entries: caches.iter().map(|cache| cache.swash.image_cache.len()).sum(),
            image_payload_bytes: caches.iter().flat_map(|cache| cache.swash.image_cache.values().flatten())
                .map(|image| image.data.capacity()).sum(),
            outline_entries: caches.iter().map(|cache| cache.swash.outline_command_cache.len()).sum(),
            outline_payload_bytes: caches.iter().flat_map(|cache| cache.swash.outline_command_cache.values().flatten())
                .map(|commands| std::mem::size_of_val(commands.as_ref())).sum(),
        }
    }

    /// Whether the database holds a face for `family`. Used to warn
    /// once per engine build that a theme names a font this machine
    /// does not have.
    fn has_family(&self, family: &str) -> bool {
        self.font_system
            .borrow()
            .db()
            .faces()
            .any(|face| face.families.iter().any(|(name, _)| name == family))
    }
}

impl Default for FontState {
    fn default() -> Self {
        Self::new()
    }
}

/// Implements `ThemeEngine` for a single `Theme`. Holds the font state
/// `cosmic-text` needs behind [`FontState`]'s `RefCell`s, because
/// `ThemeEngine`'s methods take `&self` (a theme can be shared/boxed as
/// `Box<dyn ThemeEngine>`) while shaping/rasterizing glyphs needs `&mut`
/// access — safe because the whole window manager is single-threaded.
///
/// One engine draws one theme, for the life of that theme: a restyle
/// replaces the engine (see [`RasterThemeEngine::with_fonts`] and
/// `wm_core::WindowManager::set_theme_engine`) rather than mutating it
/// underneath the layouts already derived from it. That keeps
/// "which theme is this layout from" answerable by identity — a
/// half-restyled engine, handing out old metrics and new colors in the
/// same pass, is the state this deliberately cannot reach.
pub struct RasterThemeEngine {
    theme: Theme,
    style: FrameStyle,
    system7_roles: system7::Roles,
    system7_metrics: system7::Metrics,
    scaled_system7_metrics: RefCell<HashMap<u32, system7::Metrics>>,
    fonts: FontState,
    base_scale: f32,
    scaled_themes: RefCell<HashMap<u32, Theme>>,
    title_cache: RefCell<VecDeque<(u32, DecorationRequest, DecorationBuffer)>>,
}

impl RasterThemeEngine {
    /// Builds an engine with its own freshly scanned font database.
    /// The session's *first* engine; every later one should come from
    /// [`Self::with_fonts`] so the scan is not repeated.
    pub fn new(theme: Theme) -> Self {
        Self::with_fonts(theme, FontState::new())
    }

    /// The same engine dressed in a different theme, reusing font state
    /// that is already loaded — how a live retheme or rescale builds
    /// the engine it swaps in. See [`FontState`] for why this is not
    /// simply another [`Self::new`].
    pub fn with_fonts(theme: Theme, fonts: FontState) -> Self {
        Self::with_fonts_at_scale(theme, fonts, 1.0)
    }

    /// Builds an engine whose supplied theme has already been scaled by
    /// `base_scale`. Other output scales are derived relative to that base and
    /// cached, so a mixed-DPI desk does not rescan fonts or mutate one global
    /// theme as windows cross outputs.
    pub fn with_fonts_at_scale(theme: Theme, fonts: FontState, base_scale: f32) -> Self {
        if !fonts.has_family(&theme.titlebar.font.family) {
            tracing::warn!(
                family = %theme.titlebar.font.family,
                "configured theme font not found on the system; text will render with whatever fallback sans font fontdb picks"
            );
        }
        Self {
            system7_roles: system7::Roles::from_theme(&theme),
            system7_metrics: system7::Metrics::new(base_scale),
            scaled_system7_metrics: RefCell::new(HashMap::new()),
            theme,
            style: FrameStyle::WindowMaker,
            fonts,
            base_scale: base_scale.max(0.125),
            scaled_themes: RefCell::new(HashMap::new()),
            title_cache: RefCell::new(VecDeque::new()),
        }
    }

    /// Select an implemented frame recipe. Reserved/unavailable styles return
    /// an error, never another style's pixels. Existing constructors keep their
    /// WindowMaker default and their original signatures.
    pub fn with_style(mut self, style: DecorationStyle) -> Result<Self, UnsupportedDecorationStyle> {
        self.style = FrameStyle::try_from(style)?;
        if style == DecorationStyle::System7 {
            let light = crate::default_theme::theme_variant(&self.theme.id, crate::model::Appearance::Light);
            self.system7_roles = system7::Roles::from_theme(light.as_ref().unwrap_or(&self.theme));
            if self.theme.appearance == crate::model::Appearance::Dark {
                tracing::info!("System 7 decorations use the light palette; dark appearance remains active for other surfaces");
            }
        }
        self.title_cache.get_mut().clear();
        Ok(self)
    }

    /// The actual renderer selected by this engine.
    pub fn style(&self) -> DecorationStyle {
        self.style.name()
    }

    /// Convenience constructor wrapping the flagship built-in theme.
    pub fn nextstep_classic() -> Self {
        Self::new(crate::default_theme::nextstep_classic())
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// A handle to this engine's font state, for building the engine
    /// that replaces it. Cheap: three `Rc` bumps.
    pub fn fonts(&self) -> FontState {
        self.fonts.clone()
    }
}

impl ThemeEngine for RasterThemeEngine {
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout {
        match self.style {
            FrameStyle::WindowMaker => windowmaker::layout_decoration(&self.theme, request),
            FrameStyle::System7 => system7::layout(request, self.system7_metrics),
        }
    }

    fn render(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer {
        match self.style { FrameStyle::WindowMaker => windowmaker::render_decoration(
            &self.theme,
            &mut self.fonts.font_system.borrow_mut(),
            &mut self.fonts.swash(),
            request,
            layout,
        ), FrameStyle::System7 => system7::flatten(self.render_surface(request, layout)) }
    }

    fn layout_at(&self, request: &DecorationRequest, scale: f32) -> DecorationLayout {
        if same_scale(scale, self.base_scale) {
            return self.layout(request);
        }
        if matches!(self.style, FrameStyle::System7) {
            let key = normalized_scale(scale).to_bits();
            let mut metrics = self.scaled_system7_metrics.borrow_mut();
            return system7::layout(request, *metrics.entry(key).or_insert_with(|| system7::Metrics::new(scale)));
        }
        let key = normalized_scale(scale).to_bits();
        let mut themes = self.scaled_themes.borrow_mut();
        let theme = themes.entry(key).or_insert_with(|| self.theme.scaled(scale / self.base_scale));
        windowmaker::layout_decoration(theme, request)
    }

    fn render_surface(&self, request: &DecorationRequest, layout: &DecorationLayout) -> DecorationSurface {
        match self.style { FrameStyle::WindowMaker => windowmaker::render_sparse_decoration(
            &self.theme,
            &mut self.fonts.font_system.borrow_mut(),
            &mut self.fonts.swash(),
            &mut self.title_cache.borrow_mut(),
            self.base_scale.to_bits(),
            request,
            layout,
        ), FrameStyle::System7 => system7::render_sparse(self.system7_roles, self.base_scale,
            &mut self.title_cache.borrow_mut(), &mut self.fonts.system7_fallback.borrow_mut(), request, layout) }
    }

    fn render_surface_at(
        &self,
        request: &DecorationRequest,
        layout: &DecorationLayout,
        scale: f32,
    ) -> DecorationSurface {
        if same_scale(scale, self.base_scale) {
            return self.render_surface(request, layout);
        }
        let scale = normalized_scale(scale);
        if matches!(self.style, FrameStyle::System7) {
            return system7::render_sparse(self.system7_roles, scale, &mut self.title_cache.borrow_mut(),
                &mut self.fonts.system7_fallback.borrow_mut(), request, layout);
        }
        let key = scale.to_bits();
        let mut themes = self.scaled_themes.borrow_mut();
        let theme = themes.entry(key).or_insert_with(|| self.theme.scaled(scale / self.base_scale));
        windowmaker::render_sparse_decoration(
            theme,
            &mut self.fonts.font_system.borrow_mut(),
            &mut self.fonts.swash(),
            &mut self.title_cache.borrow_mut(),
            key,
            request,
            layout,
        )
    }
}

pub(crate) fn normalized_scale(scale: f32) -> f32 {
    if scale.is_finite() { scale.max(0.125) } else { 1.0 }
}

fn same_scale(a: f32, b: f32) -> bool {
    (normalized_scale(a) - normalized_scale(b)).abs() < 0.001
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::{ButtonKind, ButtonRuntimeState, Point, ResizeEdge, Size};
    use crate::styles::windowmaker::layout_decoration;

    fn glyph_key(glyph: u16) -> cosmic_text::CacheKey {
        cosmic_text::CacheKey::new(
            cosmic_text::fontdb::ID::dummy(), glyph, 16.0, (0.0, 0.0),
            cosmic_text::fontdb::Weight::NORMAL, cosmic_text::CacheKeyFlags::empty(),
        ).0
    }

    #[test]
    fn glyph_cache_releases_large_images_and_hash_storage() {
        let mut cache = GlyphCache::new();
        let mut image = cosmic_text::SwashImage::new();
        // Capacity, not length: cached allocations still cost memory
        // when their logical payload is shorter.
        image.data = Vec::with_capacity(GLYPH_CACHE_BYTES);
        cache.swash.image_cache.insert(glyph_key(1), Some(image));
        cache.trim();
        assert_eq!(cache.swash.image_cache.capacity(), 0);
        assert_eq!(cache.next_check, 1);
    }

    #[test]
    fn font_statistics_observe_payload_capacity_without_triggering_eviction() {
        let fonts = FontState {
            system7_fallback: Rc::new(RefCell::new(system7::Fallback::from_db("en-US".into(), cosmic_text::fontdb::Database::new()))),
            font_system: Rc::new(RefCell::new(cosmic_text::FontSystem::new_with_locale_and_db(
                "en-US".into(), cosmic_text::fontdb::Database::new(),
            ))),
            swash_cache: Rc::new(RefCell::new(GlyphCache::new())),
        };
        let mut image = cosmic_text::SwashImage::new();
        image.data = Vec::with_capacity(GLYPH_CACHE_BYTES);
        let capacity = image.data.capacity();
        fonts.swash_cache.borrow_mut().swash.image_cache.insert(glyph_key(1), Some(image));
        let first = fonts.cache_statistics();
        assert_eq!(first.available_faces, 0);
        assert_eq!(first.image_entries, 1);
        assert_eq!(first.image_payload_bytes, capacity);
        assert_eq!(fonts.cache_statistics(), first, "observation does not trim the cache");
        drop(fonts.swash());
        assert_eq!(fonts.cache_statistics().image_entries, 0, "normal borrowing still evicts");
    }

    #[test]
    fn glyph_cache_bounds_negative_entries_and_preserves_a_warm_small_cache() {
        let mut cache = GlyphCache::new();
        cache.swash.image_cache.insert(glyph_key(1), None);
        cache.trim();
        cache.trim();
        assert_eq!(cache.swash.image_cache.len(), 1, "ordinary warm entries survive");
        for glyph in 0..GLYPH_CACHE_ENTRIES {
            cache.swash.image_cache.insert(glyph_key(glyph as u16), None);
        }
        cache.trim();
        assert!(cache.swash.image_cache.is_empty(), "missing glyphs also need a bound");
    }

    #[test]
    fn evicting_shared_glyphs_keeps_the_font_database_and_rendered_pixels() {
        let engine = RasterThemeEngine::nextstep_classic();
        let fonts = engine.fonts();
        let request = sample_request("Terminal — 世界", true);
        let layout = engine.layout(&request);
        let before = engine.render(&request, &layout);
        let faces = fonts.system().db().faces().count();
        {
            let mut cache = fonts.swash();
            for glyph in 0..GLYPH_CACHE_ENTRIES {
                cache.image_cache.insert(glyph_key(glyph as u16), None);
            }
        }
        assert!(fonts.swash().image_cache.is_empty(), "a clone shares the eviction");
        assert_eq!(fonts.system().db().faces().count(), faces);
        assert_eq!(engine.render(&request, &layout), before, "eviction only discards reproducible data");
        assert!(!fonts.swash().image_cache.is_empty(), "rendering repopulates the cache");
    }

    fn sample_request(title: &str, focused: bool) -> DecorationRequest {
        DecorationRequest {
            content_size: Size::new(300, 200),
            title: title.to_string(),
            focused,
            resizable: true,
            buttons: vec![
                ButtonRuntimeState { kind: ButtonKind::Close, hovered: false, pressed: false },
                ButtonRuntimeState { kind: ButtonKind::Maximize, hovered: false, pressed: false },
                ButtonRuntimeState { kind: ButtonKind::Miniaturize, hovered: false, pressed: false },
            ],
        }
    }

    #[test]
    fn layout_places_miniaturize_top_left_and_close_top_right() {
        // The classic sides, confirmed by reading actual screenshots —
        // not the reverse this used to assert.
        let theme = crate::default_theme::nextstep_classic();
        let layout = layout_decoration(&theme, &sample_request("xterm", true));

        let mini = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Miniaturize).unwrap().1;
        let close = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Close).unwrap().1;
        assert!(mini.pos.x < close.pos.x, "miniaturize should sit left of close");
        assert!(mini.pos.x < layout.frame_size.w as i32 / 2);
        assert!(close.pos.x > layout.frame_size.w as i32 / 2);
    }

    /// Regression test: the gap between a button and the titlebar's own
    /// corner bevel used to be a flat 3px constant that didn't grow with
    /// `Theme::scaled()` — at higher scales the (correctly scaled)
    /// titlebar bevel and the (correctly scaled) button bevel ended up
    /// touching with no visible gap of plain fill between them, reading
    /// as one thick undifferentiated light smear in that corner instead
    /// of two distinct chiseled elements.
    #[test]
    fn button_margin_leaves_a_real_gap_from_the_titlebar_bevel_at_any_scale() {
        for scale in [1.0, 2.0, 3.0] {
            let theme = crate::default_theme::nextstep_classic().scaled(scale);
            let layout = layout_decoration(&theme, &sample_request("xterm", true));
            let close = layout.button_hitboxes.iter().find(|(k, _)| *k == ButtonKind::Close).unwrap().1;

            let border = theme.border.width as i32;
            let bevel_inner_edge = border + theme.titlebar.bevel.width as i32;
            let gap = close.pos.x - bevel_inner_edge;
            assert!(
                gap >= theme.titlebar.bevel.width as i32,
                "scale {scale}: only {gap}px between the titlebar's own bevel and the close button — bevels touch/overlap instead of leaving a visible gap"
            );
        }
    }

    #[test]
    fn layout_frame_size_accounts_for_chrome() {
        let theme = crate::default_theme::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = layout_decoration(&theme, &request);

        assert!(layout.frame_size.w >= request.content_size.w);
        assert!(layout.frame_size.h > request.content_size.h, "titlebar/border/resize-bar must add height");
    }

    #[test]
    fn render_produces_a_correctly_sized_nonempty_buffer() {
        let engine = RasterThemeEngine::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);

        let buffer = engine.render(&request, &layout);

        assert_eq!(buffer.width, layout.frame_size.w);
        assert_eq!(buffer.height, layout.frame_size.h);
        assert_eq!(buffer.pixels.len(), (buffer.width * buffer.height * 4) as usize);
        // Not every pixel the same color — proves the gradient/bevel/text
        // pipeline actually drew something rather than leaving a flat or
        // empty buffer.
        let first = &buffer.pixels[0..4];
        assert!(
            buffer.pixels.as_chunks::<4>().0.iter().any(|px| px.as_slice() != first),
            "decoration should not be a single flat color"
        );
        assert!(
            buffer
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255),
            "server decorations are an opaque plane and may be submitted as one"
        );
    }

    #[test]
    fn sparse_storage_follows_the_chrome_perimeter_not_the_client_area() {
        let engine = RasterThemeEngine::nextstep_classic();
        let mut request = sample_request("large terminal", true);
        request.content_size = Size::new(1600, 1000);
        let layout = engine.layout(&request);
        let surface = engine.render_surface(&request, &layout);
        let full_bytes = layout.frame_size.w as usize * layout.frame_size.h as usize * 4;

        assert_eq!(surface.frame_size, layout.frame_size);
        assert_eq!(surface.parts.len(), 4, "top, bottom, left and right chrome bands");
        assert!(
            surface.retained_bytes() < full_bytes / 8,
            "{} sparse bytes should be perimeter-sized, not {} full-frame bytes",
            surface.retained_bytes(),
            full_bytes
        );
        for part in &surface.parts {
            assert_eq!(
                part.buffer.pixels.len(),
                part.buffer.width as usize * part.buffer.height as usize * 4
            );
        }
    }

    #[test]
    fn one_engine_selects_the_output_scale_without_rescanning_fonts() {
        let base = crate::default_theme::nextstep_classic();
        let fonts = FontState::new();
        let engine = RasterThemeEngine::with_fonts_at_scale(base.scaled(2.0), fonts, 2.0);
        let request = sample_request("mixed dpi", true);
        let one_x = engine.layout_at(&request, 1.0);
        let two_x = engine.layout_at(&request, 2.0);

        assert!(two_x.titlebar_height > one_x.titlebar_height);
        assert!(two_x.client_offset.y > one_x.client_offset.y);
        assert_eq!(engine.layout_at(&request, 1.0), one_x, "cached scale variants are stable");
    }

    #[test]
    fn changing_only_content_height_reuses_the_shaped_title_band() {
        let engine = RasterThemeEngine::nextstep_classic();
        let request = sample_request("shape me once", true);
        let layout = engine.layout(&request);
        let _ = engine.render_surface(&request, &layout);
        assert_eq!(engine.title_cache.borrow().len(), 1);

        let mut taller = request.clone();
        taller.content_size.h += 400;
        let taller_layout = engine.layout(&taller);
        let _ = engine.render_surface(&taller, &taller_layout);
        assert_eq!(
            engine.title_cache.borrow().len(),
            1,
            "title, font scale and available width are unchanged"
        );
    }

    /// Regression test: the resize-corner grip marks (the visual half
    /// of the resize affordance — `set_frame_cursor`'s hover-driven
    /// cursor change is the other half, in `wm-x11`) must actually
    /// render something distinguishable from the plain resize bar fill,
    /// and only when the window is resizable at all.
    #[test]
    fn resizable_windows_render_a_visible_grip_in_the_resize_corner() {
        let engine = RasterThemeEngine::nextstep_classic();
        let resizable = sample_request("xterm", true);
        let layout = engine.layout(&resizable);
        let buffer = engine.render(&resizable, &layout);

        let se = layout.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        let mut region_pixels = Vec::new();
        for y in se.pos.y..(se.pos.y + se.size.h as i32) {
            for x in se.pos.x..(se.pos.x + se.size.w as i32) {
                let idx = ((y as u32 * buffer.width + x as u32) * 4) as usize;
                region_pixels.push(&buffer.pixels[idx..idx + 4]);
            }
        }
        let first = region_pixels[0];
        assert!(region_pixels.iter().any(|px| *px != first), "the SE resize corner should show grip marks, not a flat fill");

        // A non-resizable window has no resize bar at all, so no
        // SE/SW hitboxes to draw a grip into in the first place.
        let mut non_resizable = sample_request("xterm", true);
        non_resizable.resizable = false;
        let non_resizable_layout = engine.layout(&non_resizable);
        assert!(non_resizable_layout.resize_hitboxes.is_empty());
    }

    /// The shade (windowshade / roll-up) render. `wm-core`'s
    /// `shaded_paint_inputs` asks for exactly this shape — the theme's
    /// own `shaded_frame_height` with `resizable` cleared — and the
    /// result has to be a *whole* decoration at that height, not the top
    /// slice of a taller one: the Wayland compositor draws the
    /// decoration buffer at the buffer's own size with nothing to clip
    /// it, so anything the theme paints outside those rows is what the
    /// user sees on screen.
    ///
    /// The `resizable` half is the part that isn't obvious. At shaded
    /// height there is no room below the titlebar for a resize bar, so a
    /// theme still told to draw one puts it straight over the titlebar's
    /// bottom edge — which is why the bottom border strip is checked
    /// against the top one rather than merely checking the height.
    #[test]
    fn a_shaded_frame_renders_as_a_complete_titlebar_only_decoration() {
        let engine = RasterThemeEngine::nextstep_classic();
        let border = engine.theme().border.width as u32;
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);
        assert_eq!(
            layout.shaded_frame_height,
            layout.titlebar_height + border * 2,
            "the shade keeps the titlebar and its own top/bottom border, nothing else"
        );

        let full = engine.render(&request, &layout);

        let mut shaded_request = request.clone();
        shaded_request.resizable = false;
        let mut shaded_layout = layout.clone();
        shaded_layout.frame_size.h = layout.shaded_frame_height;
        shaded_layout.resize_hitboxes.clear();
        let shaded = engine.render(&shaded_request, &shaded_layout);

        assert_eq!(shaded.width, layout.frame_size.w);
        assert_eq!(shaded.height, layout.shaded_frame_height);
        assert_eq!(shaded.pixels.len(), (shaded.width * shaded.height * 4) as usize);

        let row = |buffer: &DecorationBuffer, y: u32| {
            let stride = (buffer.width * 4) as usize;
            buffer.pixels[y as usize * stride..(y as usize + 1) * stride].to_vec()
        };

        // Everything above the bottom border — titlebar fill, bevel
        // segments, title text, button glyphs — is pixel-for-pixel the
        // decoration the unshaded frame draws in the same rows. A shade
        // is the same titlebar, not a redrawn one.
        for y in 0..(layout.shaded_frame_height - border) {
            assert_eq!(row(&shaded, y), row(&full, y), "shaded row {y} differs from the unshaded frame's titlebar");
        }
        // And the rows the resize bar would have landed in are plain
        // border, identical to the top border strip.
        for y in 0..border {
            assert_eq!(
                row(&shaded, layout.shaded_frame_height - border + y),
                row(&shaded, y),
                "the shade's bottom border strip is not plain border — something (the resize bar) painted into it"
            );
        }
    }

    /// The OS X-style zones: every edge and corner of a resizable frame
    /// must be reachable, with the extreme corner pixels resolving to a
    /// *corner* (diagonal) resize rather than the adjacent edge band —
    /// the corner arms are pushed ahead of the edge bands and hit-tests
    /// take the first match, which is what this pins down.
    #[test]
    fn all_eight_resize_zones_are_exposed_and_corners_win_at_the_extremes() {
        let engine = RasterThemeEngine::nextstep_classic();
        let request = sample_request("xterm", true);
        let layout = engine.layout(&request);

        for edge in [
            ResizeEdge::North,
            ResizeEdge::South,
            ResizeEdge::East,
            ResizeEdge::West,
            ResizeEdge::NorthEast,
            ResizeEdge::NorthWest,
            ResizeEdge::SouthEast,
            ResizeEdge::SouthWest,
        ] {
            assert!(
                layout.resize_hitboxes.iter().any(|(e, _)| *e == edge),
                "resizable frame should expose a {edge:?} zone"
            );
        }

        // Same first-match rule `wm-core`'s hit_test applies.
        let first_edge_at = |p: Point| layout.resize_hitboxes.iter().find(|(_, r)| r.contains(p)).map(|(e, _)| *e);
        let w = layout.frame_size.w as i32;
        let h = layout.frame_size.h as i32;
        assert_eq!(first_edge_at(Point::new(0, 0)), Some(ResizeEdge::NorthWest));
        assert_eq!(first_edge_at(Point::new(w - 1, 0)), Some(ResizeEdge::NorthEast));
        assert_eq!(first_edge_at(Point::new(w / 2, 0)), Some(ResizeEdge::North));
        assert_eq!(first_edge_at(Point::new(0, h / 2)), Some(ResizeEdge::West));
        assert_eq!(first_edge_at(Point::new(w - 1, h / 2)), Some(ResizeEdge::East));
        // The titlebar's center must stay a drag region, not a resize.
        assert_eq!(first_edge_at(Point::new(w / 2, layout.titlebar_height as i32 / 2 + 4)), None);
    }

    /// Regression test: the resize-corner hitbox used to be a flat 10px
    /// literal that never grew with `Theme::scaled()` — at higher
    /// `CHONKSTEP_SCALE` values the visible grip mark (drawn from this
    /// same hitbox) still scaled up correctly, but the actual clickable/
    /// hoverable area you had to land the cursor on stayed a tiny,
    /// proportionally shrinking target, which is exactly what "have to
    /// be extremely precise with the mouse to get the resize cursor"
    /// felt like in practice.
    #[test]
    fn resize_corner_hitbox_grows_with_scale() {
        let theme = crate::default_theme::nextstep_classic();
        let scaled = theme.scaled(3.0);
        let layout_1x = layout_decoration(&theme, &sample_request("xterm", true));
        let layout_3x = layout_decoration(&scaled, &sample_request("xterm", true));

        let se_1x = layout_1x.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        let se_3x = layout_3x.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1;
        assert!(se_3x.size.w > se_1x.size.w * 2, "the corner hitbox should scale up with the rest of the chrome, not stay fixed");
    }

    #[test]
    fn focused_and_unfocused_render_differently() {
        let engine = RasterThemeEngine::nextstep_classic();
        let focused = sample_request("xterm", true);
        let unfocused = sample_request("xterm", false);
        let layout = engine.layout(&focused);

        let a = engine.render(&focused, &layout);
        let b = engine.render(&unfocused, &layout);

        assert_ne!(a.pixels, b.pixels, "focused/inactive titlebar fills differ in the flagship theme");
    }

    #[test]
    fn title_text_actually_renders_between_the_buttons() {
        // Regression test: `leftmost_button`/`rightmost_button` used to
        // take a blind max()/min() over every button's edges combined,
        // which — since Close sits left of Miniaturize — actually picked
        // Miniaturize's (rightmost) right edge as "leftmost" and Close's
        // (leftmost) left edge as "rightmost", collapsing the title's
        // available width to zero. A titlebar with a real title rendered
        // byte-for-byte identical to one with an empty title (proven by
        // a from-scratch diff test, not a "some pixel differs somewhere"
        // check, since bevel/button highlight pixels alone are enough to
        // make a weaker check pass without any text ever drawing).
        let engine = RasterThemeEngine::nextstep_classic();
        let empty = sample_request("", true);
        let titled = sample_request("HELLOWORLD", true);
        let layout = engine.layout(&empty);
        assert_eq!(layout, engine.layout(&titled), "title text must never affect layout/hit-test geometry");

        let empty_buffer = engine.render(&empty, &layout);
        let titled_buffer = engine.render(&titled, &layout);

        assert_ne!(empty_buffer.pixels, titled_buffer.pixels, "a non-empty title must actually paint glyph pixels between the titlebar buttons");
    }

    #[test]
    fn long_title_ink_stays_inside_the_titlebar() {
        let engine = RasterThemeEngine::nextstep_classic();
        let empty = sample_request("", true);
        let long = sample_request(
            "iconidentify/chonkstep: A traditional floating compositor for Omarchy, designed as a drop-in replacement for Hyprland. - Google Chrome",
            true,
        );
        let layout = engine.layout(&empty);
        let empty_buffer = engine.render(&empty, &layout);
        let long_buffer = engine.render(&long, &layout);
        let border = engine.theme().border.width as u32;
        let first_title_row = border;
        let after_title_row = border + layout.titlebar_height;

        let changed_rows: Vec<u32> = (0..long_buffer.height)
            .filter(|y| {
                let start = (*y * long_buffer.width * 4) as usize;
                let end = start + (long_buffer.width * 4) as usize;
                long_buffer.pixels[start..end] != empty_buffer.pixels[start..end]
            })
            .collect();
        assert!(!changed_rows.is_empty(), "the long title must render visible ink");
        assert!(
            changed_rows.iter().all(|row| *row >= first_title_row && *row < after_title_row),
            "title ink escaped rows {first_title_row}..{after_title_row}: changed rows were {changed_rows:?}"
        );
    }

    #[test]
    fn pressed_button_renders_differently_from_unpressed() {
        let engine = RasterThemeEngine::nextstep_classic();
        let mut request = sample_request("xterm", true);
        let layout = engine.layout(&request);
        let unpressed = engine.render(&request, &layout);

        request.buttons[0].pressed = true; // Close
        let pressed = engine.render(&request, &layout);

        assert_ne!(unpressed.pixels, pressed.pixels, "a pressed button must render its sunken bevel, not the same pixels");
    }
}
