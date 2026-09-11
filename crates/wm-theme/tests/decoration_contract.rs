//! The pre-style renderer's actual allocation and sparse-storage contract.
use chonk_test_support::{measure, AllocationCounter};
use wm_theme::{DecorationStyle, FontState, RasterThemeEngine, SUPPORTED_DECORATION_STYLES};
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationRequest, Size, ThemeEngine};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

fn engine(style: DecorationStyle, fonts: &FontState) -> RasterThemeEngine {
    RasterThemeEngine::with_fonts(wm_theme::default_theme::nextstep_classic(), fonts.clone()).with_style(style).unwrap()
}

fn request(size: Size) -> DecorationRequest {
    DecorationRequest {
        content_size: size,
        title: "Terminal — compositor performance".into(),
        focused: true,
        resizable: true,
        buttons: Vec::new(),
    }
}

#[test]
fn layout_and_warm_render_do_not_exceed_the_pre_style_allocation_budget() {
    let fonts = FontState::new();
    for &style in SUPPORTED_DECORATION_STYLES {
        let engine = engine(style, &fonts);
        for (size, budgets) in [
            (Size::new(800, 600), [(110664, 142974), (221856, 286406)]),
            (Size::new(1280, 800), [(175624, 227134), (351776, 454726)]),
            (Size::new(2560, 1600), [(350984, 453694), (702496, 907846)]),
        ] {
            for (scale, (retained, allocated)) in [1.0, 2.0].into_iter().zip(budgets) {
                let request = request(size);
                let layout = engine.layout_at(&request, scale);
                let first = engine.render_surface_at(&request, &layout, scale);
                let (repeated_layout, layout_allocations) = measure(|| engine.layout_at(&request, scale));
                let (second, render_allocations) = measure(|| engine.render_surface_at(&request, &layout, scale));
                assert_eq!(repeated_layout, layout);
                assert_eq!(first, second);
                eprintln!("{style:?} {size:?} scale {scale}: layout={layout_allocations:?}, render={render_allocations:?}, retained={}", first.retained_bytes());
                // These owned Vec APIs allocate today. Pin the measured main
                // budget instead of misrepresenting a cache hit as allocation-free.
                assert!(layout_allocations.calls <= 4, "{layout_allocations:?}");
                assert!(layout_allocations.requested_bytes <= 600, "{layout_allocations:?}");
                assert!(render_allocations.calls <= 8, "{render_allocations:?}");
                assert!(render_allocations.requested_bytes <= allocated, "{render_allocations:?}");
                assert!(first.retained_bytes() <= retained);
            }
        }
    }
}

#[test]
fn every_frame_state_retains_only_nonoverlapping_perimeter_bands() {
    let fonts = FontState::new();
    for &style in SUPPORTED_DECORATION_STYLES {
        let engine = engine(style, &fonts);
        for size in [Size::new(800, 600), Size::new(1280, 800), Size::new(2560, 1600)] {
            for scale in [1.0, 1.5, 2.0] {
                for focused in [false, true] {
                    for resizable in [false, true] {
                        for shaded in [false, true] {
                            for pressed in [None, Some(ButtonKind::Close), Some(ButtonKind::Miniaturize)] {
                                let mut request = request(size);
                                request.focused = focused;
                                request.resizable = resizable;
                                request.buttons = pressed.into_iter().map(|kind| ButtonRuntimeState {
                                    kind, hovered: false, pressed: true,
                                }).collect();
                                let mut layout = engine.layout_at(&request, scale);
                                if shaded {
                                    request.content_size.h = 0;
                                    request.resizable = false;
                                    layout.frame_size.h = layout.shaded_frame_height;
                                    layout.resize_hitboxes.clear();
                                }
                                let surface = engine.render_surface_at(&request, &layout, scale);
                                assert_eq!(surface.frame_size, layout.frame_size);
                                let content = (
                                    i64::from(layout.client_offset.x), i64::from(layout.client_offset.y),
                                    i64::from(layout.client_offset.x) + i64::from(request.content_size.w),
                                    i64::from(layout.client_offset.y) + i64::from(request.content_size.h),
                                );
                                let mut regions = Vec::new();
                                for part in &surface.parts {
                                    let rect = (
                                        i64::from(part.offset.x), i64::from(part.offset.y),
                                        i64::from(part.offset.x) + i64::from(part.buffer.width),
                                        i64::from(part.offset.y) + i64::from(part.buffer.height),
                                    );
                                    assert!(rect.0 >= 0 && rect.1 >= 0);
                                    assert!(rect.2 <= i64::from(layout.frame_size.w));
                                    assert!(rect.3 <= i64::from(layout.frame_size.h));
                                    assert!(!overlaps(rect, content), "chrome intersects the client's pixels");
                                    assert!(regions.iter().all(|&other| !overlaps(rect, other)), "chrome bands overlap");
                                    assert_eq!(part.buffer.pixels.len(), part.buffer.width as usize * part.buffer.height as usize * 4);
                                    regions.push(rect);
                                }
                                let perimeter = 2 * (layout.frame_size.w as usize + layout.frame_size.h as usize);
                                let largest_band = layout.client_offset.y.max(1) as usize;
                                assert!(surface.retained_bytes() <= perimeter * largest_band * 4);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn overlaps(a: (i64, i64, i64, i64), b: (i64, i64, i64, i64)) -> bool {
    a.0.max(b.0) < a.2.min(b.2) && a.1.max(b.1) < a.3.min(b.3)
}
