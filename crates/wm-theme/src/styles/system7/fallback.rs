//! A startup-selected resident fallback set, sharing the session's discovered
//! font database. Only already opened font sources enter the render-time set.
use cosmic_text::{Attrs, Buffer, CacheKeyFlags, Family, FontSystem, Metrics, Shaping,
    SwashContent, Weight, Wrap, fontdb::Source};
use super::super::super::raster::GlyphCache;

pub(crate) struct Fallback { fonts: FontSystem, cache: GlyphCache }

impl Fallback {
    pub(crate) fn cache(&self) -> &GlyphCache { &self.cache }

    #[cfg(test)]
    pub(crate) fn from_db(locale: String, db: cosmic_text::fontdb::Database) -> Self {
        Self::prepare(&mut FontSystem::new_with_locale_and_db(locale, db))
    }

    pub(crate) fn prepare(fonts: &mut FontSystem) -> Self {
        // Warm the session's own font selection once, before the event loop.
        // The fallback renderer receives only opened sources, so a newly
        // encountered title cannot trigger a file access during rendering.
        if fonts.db().faces().next().is_some() {
            let mut probe = Buffer::new(fonts, Metrics::new(12.0, 15.0));
            probe.set_wrap(Wrap::None);
            probe.set_text("Aa — Жж Ελληνικά 中文 日本語 한글 العربية עברית हिन्दी বাংলা தமிழ் ไทย ქართული Հայերեն ሀ",
                &Attrs::new().family(Family::SansSerif).weight(Weight::BOLD), Shaping::Advanced, None);
            probe.shape_until_scroll(fonts, false);
        }
        let locale = fonts.locale().to_owned();
        // Copy just resident face records, not the full installed database
        // followed by hundreds of removals and an oversized retained slot map.
        let mut db = cosmic_text::fontdb::Database::new();
        db.set_serif_family(fonts.db().family_name(&Family::Serif));
        db.set_sans_serif_family(fonts.db().family_name(&Family::SansSerif));
        db.set_monospace_family(fonts.db().family_name(&Family::Monospace));
        db.set_cursive_family(fonts.db().family_name(&Family::Cursive));
        db.set_fantasy_family(fonts.db().family_name(&Family::Fantasy));
        for face in fonts.db().faces().filter(|face| !matches!(face.source, Source::File(_))) {
            db.push_face_info(face.clone());
        }
        // Touch selected mappings during initialization. They are shared by
        // FontSystem, so this does not copy every installed font into the heap.
        for face in db.faces() {
            match &face.source {
                Source::Binary(data) | Source::SharedFile(_, data) => {
                    let bytes = data.as_ref().as_ref();
                    for page in bytes.chunks(4096) { std::hint::black_box(page[0]); }
                }
                Source::File(_) => unreachable!("unopened fallback font retained"),
            }
        }
        Self { fonts: FontSystem::new_with_locale_and_db(locale, db), cache: GlyphCache::new() }
    }

    /// One shaped Unicode run at the 1x source resolution. Replication happens
    /// in the frame painter, keeping integer output scales exactly nearest.
    pub(super) fn render(&mut self, text: &str, max_width: u32) -> Mask {
        if self.fonts.db().faces().next().is_none() {
            let width = (text.chars().count().saturating_mul(7) as u32).max(1).min(max_width.max(1));
            let mut mask = Mask { width, pixels: vec![false; width as usize * 15] };
            for x in (0..width).step_by(7) { mask.missing(x as i32, 7); }
            return mask;
        }
        let mut buffer = Buffer::new(&mut self.fonts, Metrics::new(12.0, 15.0));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &Attrs::new().family(Family::SansSerif).weight(Weight::BOLD)
            .cache_key_flags(CacheKeyFlags::DISABLE_HINTING), Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.fonts, false);
        let width = buffer.layout_runs().map(|run| run.line_w.ceil() as u32).max().unwrap_or(0).max(1).min(max_width.max(1));
        let mut mask = Mask { width, pixels: vec![false; width as usize * 15] };
        self.cache.trim();
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let physical = glyph.physical((0.0, 0.0), 1.0);
                let missing = glyph.glyph_id == 0;
                if missing {
                    mask.missing(glyph.x.round() as i32, glyph.w.ceil() as u32);
                    continue;
                }
                let Some(image) = self.cache.swash.get_image(&mut self.fonts, physical.cache_key) else {
                    mask.missing(glyph.x.round() as i32, glyph.w.ceil() as u32);
                    continue;
                };
                for y in 0..image.placement.height {
                    for x in 0..image.placement.width {
                        let i = (y * image.placement.width + x) as usize;
                        let coverage = match image.content {
                            SwashContent::Mask => image.data[i],
                            SwashContent::Color => image.data[i * 4 + 3],
                            SwashContent::SubpixelMask => image.data[i * 4..i * 4 + 3].iter().copied().max().unwrap_or(0),
                        };
                        if coverage >= 128 {
                            mask.set(physical.x + image.placement.left + x as i32,
                                12 + physical.y - image.placement.top + y as i32);
                        }
                    }
                }
            }
        }
        mask
    }
}

pub(super) struct Mask { pub width: u32, pub pixels: Vec<bool> }

impl Mask {
    fn set(&mut self, x: i32, y: i32) {
        if x >= 0 && x < self.width as i32 && (0..15).contains(&y) {
            self.pixels[y as usize * self.width as usize + x as usize] = true;
        }
    }
    fn missing(&mut self, x: i32, width: u32) {
        let width = width.max(5).min(self.width);
        for y in 3..12 {
            for dx in 0..width.saturating_sub(1) {
                if y == 3 || y == 11 || dx == 0 || dx + 2 == width { self.set(x + dx as i32, y); }
            }
        }
    }
}
