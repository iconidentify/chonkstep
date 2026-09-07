//! A thumbnail must not allocate a copy of its source window's pixels.
use chonk_test_support::{measure, AllocationCounter};
use wm_theme::icon::render_icon_tile;
use wm_theme_api::DecorationBuffer;

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

#[test]
fn drawing_a_small_icon_does_not_copy_the_full_preview() {
    let theme = wm_theme::default_theme::nextstep_classic();
    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let preview = DecorationBuffer {
        width: 1920,
        height: 1080,
        pixels: vec![255; 1920 * 1080 * 4],
    };
    // Font setup, source allocation and lazy glyph work stay outside the
    // measured operation. The normal rendered tile allocation remains counted.
    std::hint::black_box(render_icon_tile(
        &theme,
        &mut fonts,
        &mut swash,
        56,
        "Preview",
        Some(&preview),
    ));
    let (tile, allocations) = measure(|| {
        render_icon_tile(
            &theme,
            &mut fonts,
            &mut swash,
            56,
            "Preview",
            Some(&preview),
        )
    });
    assert_eq!((tile.width, tile.height), (56, 56));
    assert!(
        allocations.calls > 0,
        "the test allocator must be installed"
    );
    assert!(
        allocations.requested_bytes < preview.pixels.len(),
        "56px icon requested {} bytes against an {}-byte source; copying the source is forbidden",
        allocations.requested_bytes,
        preview.pixels.len()
    );
    eprintln!(
        "56px icon from 1080p preview: {allocations:?}; allocation requests, not live memory or RSS"
    );
}
