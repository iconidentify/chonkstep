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

use wm_theme::{instrument_panel, overview, DecorationStyle, RasterThemeEngine, SUPPORTED_DECORATION_STYLES};
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
        // SAFETY: GlobalAlloc's caller supplies a valid, nonzero layout;
        // it is forwarded unchanged to the backing System allocator.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's valid, nonzero layout is forwarded unchanged.
        // System supplies the zero initialization required by this operation.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocated(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: every allocation comes from System, and the caller must
        // supply a still-live pointer and the same layout used to allocate it.
        unsafe { System.dealloc(pointer, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller supplies a live System allocation, its original
        // layout, and a nonzero new size satisfying GlobalAlloc's size bound.
        // Forward all three unchanged; a null result leaves the old allocation live.
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
    measure_with_metadata(name, iterations, &mut work, fingerprint, "");
}

fn measure_with_metadata<T>(
    name: &str,
    iterations: usize,
    mut work: impl FnMut() -> T,
    fingerprint: impl Fn(&T) -> u64,
    metadata: &str,
) {
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
        "{{\"workload\":\"{name}\",\"iterations\":{iterations},\"ns_per_iteration\":{},\"allocations_per_iteration\":{},\"allocated_bytes_per_iteration\":{},\"peak_live_growth_bytes\":{peak},\"checksum\":\"{checksum:016x}\"{metadata}}}",
        elapsed.as_nanos() / iterations as u128, allocations / iterations, bytes / iterations,
    );
}

fn main() {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let mut style = DecorationStyle::WindowMaker;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--style" => {
                index += 1;
                let name = arguments.get(index).map(String::as_str).unwrap_or("");
                let selected = DecorationStyle::from_name(name).filter(|style| SUPPORTED_DECORATION_STYLES.contains(style));
                let Some(selected) = selected else {
                    eprintln!("unsupported style {name:?}; available: {:?}", SUPPORTED_DECORATION_STYLES);
                    std::process::exit(2);
                };
                style = selected;
            }
            "--glyph-churn" | "--decoration-matrix" | "--shell-chrome" => {}
            argument => {
                eprintln!("unknown argument {argument:?}; use --style windowmaker, --decoration-matrix, --shell-chrome or --glyph-churn");
                std::process::exit(2);
            }
        }
        index += 1;
    }
    if arguments.iter().any(|argument| argument == "--glyph-churn") {
        glyph_churn();
        return;
    }
    if arguments.iter().any(|argument| argument == "--shell-chrome") {
        shell_chrome(style);
        return;
    }
    decoration_matrix(style);
    if arguments.iter().any(|argument| argument == "--decoration-matrix") {
        return;
    }
    let theme = wm_theme::default_theme::nextstep_classic();
    let engine = RasterThemeEngine::new(theme.clone()).with_style(style).unwrap();
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

/// Event-time shell raster costs. These calls happen on open/semantic changes;
/// native Overview frames reuse their outputs instead of invoking them again.
fn shell_chrome(style: DecorationStyle) {
    let fonts = wm_theme::FontState::new();
    let items = [wm_theme::menu::MenuItem::Action { label: "Terminal".into(), action: 1 },
        wm_theme::menu::MenuItem::Submenu { label: "Applications".into(), items: Vec::new() }];
    let entries = [wm_theme::switcher::SwitcherEntry { title: "Terminal".into(), preview: None },
        wm_theme::switcher::SwitcherEntry { title: "Notes".into(), preview: None }];
    for scale in [1.0, 2.0] {
        let theme = wm_theme::default_theme::nextstep_classic().scaled(scale);
        let chrome = wm_theme::UiChrome::new(&theme, fonts.clone(), style, scale);
        let (mut fs, mut sc) = (fonts.system(), fonts.swash());
        let metadata = format!(",\"style\":\"{}\",\"scale\":{scale}", style.name());
        let tile = (56.0 * scale) as u32;
        measure_with_metadata("shell-menu", 200,
            || chrome.menu(&theme, &mut fs, "ChonkStep", &items, Some(1), true),
            |menu| digest(&menu.buffer.pixels), &metadata);
        measure_with_metadata("shell-switcher", 200,
            || chrome.switcher(&theme, &mut fs, &mut sc, &entries, 0, tile),
            |buffer| digest(&buffer.pixels), &metadata);
        measure_with_metadata("shell-icon", 500,
            || chrome.icon(&theme, &mut fs, &mut sc, tile, "Terminal", None),
            |buffer| digest(&buffer.pixels), &metadata);
        measure_with_metadata("shell-caption", 500,
            || chrome.label(&theme, &mut fs, &mut sc, "Desktop 2 - Terminal", tile * 4, tile / 2, true),
            |buffer| digest(&buffer.pixels), &metadata);
    }
}

/// A cold render misses the engine's title cache, while font discovery, glyph
/// warming, engine construction and per-scale setup remain outside the interval.
/// The warm case calls the actual owned-buffer API: its copies/allocations count.
fn decoration_matrix(style: DecorationStyle) {
    let theme = wm_theme::default_theme::nextstep_classic();
    let fonts = wm_theme::FontState::new();
    for size in [Size::new(800, 600), Size::new(1280, 800), Size::new(2560, 1600)] {
        for scale in [1.0, 2.0] {
            let engine = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone()).with_style(style).unwrap();
            let request = DecorationRequest {
                content_size: size,
                title: "Terminal — compositor performance".into(),
                focused: true,
                resizable: true,
                buttons: Vec::new(),
            };
            let layout = engine.layout_at(&request, scale);
            let surface = engine.render_surface_at(&request, &layout, scale);
            let retained = surface.retained_bytes();
            let metadata = format!(
                ",\"style\":\"{}\",\"width\":{},\"height\":{},\"scale\":{scale},\"retained_bytes\":{retained}",
                style.name(), size.w, size.h,
            );
            let fingerprint = |surface: &wm_theme_api::DecorationSurface| {
                surface.parts.iter().fold(0, |hash, part| hash ^ digest(&part.buffer.pixels))
            };
            measure_with_metadata("layout", 100_000,
                || engine.layout_at(black_box(&request), scale),
                |layout| u64::from(layout.frame_size.w) << 32 | u64::from(layout.frame_size.h),
                &metadata);
            measure_with_metadata("render-warm", 1000,
                || engine.render_surface_at(black_box(&request), &layout, scale),
                fingerprint, &metadata);
            // One disposable engine per render, plus the untimed checksum call.
            // Nothing is constructed inside the measured closure.
            let engines: Vec<_> = (0..101).map(|_| {
                let engine = RasterThemeEngine::with_fonts(theme.clone(), fonts.clone()).with_style(style).unwrap();
                black_box(engine.layout_at(&request, scale));
                engine
            }).collect();
            let mut engines = engines.iter();
            measure_with_metadata("render-cold", 100,
                || engines.next().unwrap().render_surface_at(black_box(&request), &layout, scale),
                fingerprint, &metadata);
        }
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
