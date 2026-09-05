//! Repeatable CPU raster and text workloads with Rust allocation accounting.
//!
//! Run `cargo run --release -p wm-theme --example performance`. Save the
//! executable before modifying the renderer to compare identical workloads.
//! These counters cover Rust's allocator, not allocations inside system
//! libraries; process memory remains a separate compositor measurement.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

use wm_theme::{instrument_panel, overview, RasterThemeEngine};
use wm_theme_api::{DecorationRequest, Size, ThemeEngine};

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

fn allocated(bytes: usize) {
    ALLOCATIONS.fetch_add(1, Relaxed);
    ALLOCATED_BYTES.fetch_add(bytes, Relaxed);
    let live = LIVE_BYTES.fetch_add(bytes, Relaxed) + bytes;
    PEAK_BYTES.fetch_max(live, Relaxed);
}

// SAFETY: Every operation is forwarded to System with its original pointer
// and layout. Accounting only touches atomics and cannot allocate recursively.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
            allocated(new_size);
        }
        result
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}

fn measure<T>(name: &str, iterations: usize, mut work: impl FnMut() -> T, fingerprint: impl Fn(&T) -> u64) {
    // Warm font and title caches, and checksum a complete output outside
    // the timed interval. Matching checksums guard against changed pixels.
    let first = work();
    let checksum = fingerprint(&first);
    drop(first);
    let allocations = ALLOCATIONS.load(Relaxed);
    let bytes = ALLOCATED_BYTES.load(Relaxed);
    let live = LIVE_BYTES.load(Relaxed);
    PEAK_BYTES.store(live, Relaxed);
    let started = Instant::now();
    for _ in 0..iterations {
        drop(black_box(work()));
    }
    let elapsed = started.elapsed();
    let allocations = ALLOCATIONS.load(Relaxed) - allocations;
    let bytes = ALLOCATED_BYTES.load(Relaxed) - bytes;
    let peak = PEAK_BYTES.load(Relaxed).saturating_sub(live);
    println!(
        "{{\"workload\":\"{name}\",\"iterations\":{iterations},\"ns_per_iteration\":{},\"allocations_per_iteration\":{},\"allocated_bytes_per_iteration\":{},\"peak_live_growth_bytes\":{peak},\"checksum\":\"{checksum:016x}\"}}",
        elapsed.as_nanos() / iterations as u128, allocations / iterations, bytes / iterations,
    );
}

fn main() {
    if std::env::args().any(|argument| argument == "--glyph-churn") {
        glyph_churn();
        return;
    }
    let theme = wm_theme::default_theme::nextstep_classic();
    let engine = RasterThemeEngine::new(theme.clone());
    let request = DecorationRequest {
        content_size: Size::new(1600, 1000), title: "Terminal — compositor performance".into(),
        focused: true, resizable: true, buttons: Vec::new(),
    };
    let layout = engine.layout(&request);
    measure("decoration-1600x1000", 100, || engine.render(&request, &layout), |output| digest(&output.pixels));
    measure("sparse-decoration-1600x1000", 1000,
        || engine.render_surface(&request, &layout),
        |output| output.parts.iter().fold(0, |hash, part| hash ^ digest(&part.buffer.pixels)));
    measure("clock-224", 1000, || wm_theme::clock::render_clock_tile(&theme, 224, 10, 9, 30),
        |output| digest(&output.pixels));

    let mut fonts = cosmic_text::FontSystem::new();
    let mut swash = cosmic_text::SwashCache::new();
    let overview_layout = overview::layout(Size::new(1920, 1080), 56, overview::header_height(&theme), 0, 4);
    measure("overview-empty-1920x1080", 50,
        || overview::render_overview(&theme, &mut fonts, &mut swash, &[], (0, 4), &overview_layout),
        |output| digest(&output.pixels));

    let style = instrument_panel::PanelStyle::new(&theme);
    let font = style.typeface(instrument_panel::TypeRole::Row, 28);
    for (length, iterations) in [(64, 100), (1024, 5), (4096, 1)] {
        let text = "Peripheral ".repeat(length / 11 + 1);
        let text = &text[..length];
        measure(&format!("panel-label-{length}"), iterations,
            || instrument_panel::fit_type(&mut fonts, &font, text, 160),
            |output| digest(output.as_bytes()));
    }
}

/// Exercise the shared session cache across unique Unicode labels and
/// eight font sizes (theme / output-scale changes), without retaining
/// any rendered image. This is a cache stress test, not a typical title.
fn glyph_churn() {
    use wm_theme::model::{Color, TextAlign};

    let fonts = wm_theme::FontState::new();
    let mut font = wm_theme::default_theme::nextstep_classic().titlebar.font;
    let mut pixmap = tiny_skia::Pixmap::new(1024, 128).unwrap();
    for phase in 0..16 {
        font.size = 16.0 + (phase % 8) as f32 * 8.0;
        let started = Instant::now();
        for label in 0..600 {
            let text: String = (0..12).map(|offset| {
                char::from_u32(0x4e00 + ((label * 12 + offset) % 0x5000)).unwrap()
            }).collect();
            pixmap.fill(tiny_skia::Color::BLACK);
            wm_theme::paint::draw_text(
                &mut pixmap, &mut fonts.system(), &mut fonts.swash(), &text,
                &font, Color::rgb(255, 255, 255), 0, 0, 1024, 128, TextAlign::Left,
            );
        }
        let elapsed = started.elapsed();
        let cache = fonts.swash();
        let images = cache.image_cache.len();
        let image_bytes: usize = cache.image_cache.values().flatten().map(|image| image.data.len()).sum();
        let rss_kib = std::fs::read_to_string("/proc/self/smaps_rollup").ok().and_then(|text| {
            text.lines().find_map(|line| line.strip_prefix("Rss:")?.split_whitespace().next()?.parse::<u64>().ok())
        }).unwrap_or(0);
        println!(
            "{{\"workload\":\"glyph-churn\",\"phase\":{phase},\"font_size\":{},\"renders\":600,\"elapsed_ms\":{},\"cached_images\":{images},\"cached_image_bytes\":{image_bytes},\"rust_live_bytes\":{},\"rss_kib\":{rss_kib},\"checksum\":\"{:016x}\"}}",
            font.size, elapsed.as_secs_f64() * 1000.0, LIVE_BYTES.load(Relaxed), digest(pixmap.data()),
        );
    }
}
