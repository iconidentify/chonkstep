//! Real named themes inherit the pre-style heap/storage ceilings, not merely
//! Modern's fallback recipe applied to the old classic palette.
use chonk_test_support::{measure, AllocationCounter};
use wm_theme::{DecorationStyle, FontState, RasterThemeEngine};
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationRequest, Size, ThemeEngine};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

#[test]
fn all_named_modern_palettes_and_controls_fit_the_existing_allocation_budgets() {
    let fonts = FontState::new();
    for (name, _) in wm_theme::modern::CHOICES {
        for appearance in [wm_theme::Appearance::Light, wm_theme::Appearance::Dark] {
            let engine = RasterThemeEngine::with_fonts(
                wm_theme::modern::theme(name, appearance).unwrap(),
                fonts.clone(),
            )
            .with_style(DecorationStyle::Modern)
            .unwrap();
            for (size, budgets) in [
                (Size::new(800, 600), [(110664, 142974), (221856, 286406)]),
                (Size::new(1280, 800), [(175624, 227134), (351776, 454726)]),
                (Size::new(2560, 1600), [(350984, 453694), (702496, 907846)]),
            ] {
                for (scale, (retained, allocated)) in [1.0, 2.0].into_iter().zip(budgets) {
                    for focused in [false, true] {
                        for pressed in [None, Some(ButtonKind::Close), Some(ButtonKind::Maximize)] {
                            let request = DecorationRequest {
                                content_size: size,
                                title: "Terminal — compositor performance".into(),
                                focused,
                                resizable: true,
                                buttons: pressed
                                    .into_iter()
                                    .map(|kind| ButtonRuntimeState {
                                        kind,
                                        hovered: true,
                                        pressed: true,
                                    })
                                    .collect(),
                            };
                            let layout = engine.layout_at(&request, scale);
                            let first = engine.render_surface_at(&request, &layout, scale);
                            let (_, layout_cost) = measure(|| engine.layout_at(&request, scale));
                            let (next, cost) =
                                measure(|| engine.render_surface_at(&request, &layout, scale));
                            assert_eq!(first, next);
                            assert!(
                                layout_cost.calls <= 4 && layout_cost.requested_bytes <= 600,
                                "{layout_cost:?}"
                            );
                            assert!(
                                cost.calls <= 8 && cost.requested_bytes <= allocated,
                                "{name} {appearance:?} {size:?}/{scale} {pressed:?}: {cost:?}"
                            );
                            assert!(first.retained_bytes() <= retained,
                                "{name} {appearance:?} {size:?}/{scale} {pressed:?}: {} > {retained}",first.retained_bytes());
                            assert!(first.parts.len() <= 4);
                            assert!(!first.solids.is_empty());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn modern_offline_geometry_is_bounded_before_any_pixel_allocation() {
    let engine = RasterThemeEngine::nextstep_classic()
        .with_style(DecorationStyle::Modern)
        .unwrap();
    for content_size in [
        Size::new(0, 0),
        Size::new(1, 1),
        Size::new(u32::MAX, u32::MAX),
    ] {
        let request = DecorationRequest {
            content_size,
            title: String::new(),
            focused: true,
            resizable: true,
            buttons: Vec::new(),
        };
        let layout = engine.layout(&request);
        assert!(layout.frame_size.w <= 8480 && layout.frame_size.h <= 8976);
        for (_, rect) in layout.button_hitboxes.iter() {
            assert!(rect.pos.x >= 0 && rect.pos.y >= 0);
            assert!(rect.pos.x as u32 + rect.size.w <= layout.frame_size.w);
            assert!(rect.pos.y as u32 + rect.size.h <= layout.frame_size.h);
        }
        let mut forged = layout;
        forged.frame_size = Size::new(u32::MAX, u32::MAX);
        let (surface, cost) = measure(|| engine.render_surface(&request, &forged));
        assert_eq!(surface.retained_bytes(), 0);
        assert_eq!(cost.calls, 0);
    }
}
