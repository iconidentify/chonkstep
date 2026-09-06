//! Deterministic pixel and allocation budgets for the dock clock renderer.

use chonk_test_support::{measure, AllocationCounter};
use std::f32::consts::PI;
use tiny_skia::{LineCap, Paint, PathBuilder, Pixmap, Stroke, Transform};
use wm_theme::clock::render_clock_tile;
use wm_theme::default_theme::nextstep_classic;
use wm_theme::model::{Color, Theme};
use wm_theme::{paint, tile};

#[global_allocator]
static ALLOCATOR: AllocationCounter = AllocationCounter;

fn reference_line(
    pixmap: &mut Pixmap,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: Color,
    width: f32,
) {
    let mut path = PathBuilder::new();
    path.move_to(x0, y0);
    path.line_to(x1, y1);
    let Some(path) = path.finish() else { return };
    let mut paint = Paint::default();
    paint.set_color(paint::sk_color(color));
    paint.anti_alias = true;
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        ..Default::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// The pre-optimization renderer retained in test code as a pixel oracle.
fn reference_clock_pixels(
    theme: &Theme,
    size: u32,
    hour: u32,
    minute: u32,
    second: u32,
) -> Vec<u8> {
    let size = size.max(8);
    let mut pixmap = Pixmap::new(size, size).expect("nonzero clock tile size");
    tile::draw_tile_base(&mut pixmap, 0, 0, size, theme);
    let bevel_t = theme.tile.bevel.width.max(1);
    let inset = (bevel_t as f32 + 2.0).max(3.0);
    let well_size = (size as f32 - inset * 2.0).max(1.0) as u32;
    tile::draw_tile_well(
        &mut pixmap,
        inset as i32,
        inset as i32,
        well_size,
        well_size,
        theme,
    );
    let cx = size as f32 / 2.0;
    let cy = size as f32 / 2.0;
    let radius = (size as f32 / 2.0 - inset - bevel_t as f32 - 2.0).max(1.0);
    let ink = tile::tile_ink(theme);
    let ink_dim = tile::tile_ink_dim(theme);
    for i in 0..12 {
        let angle = i as f32 * (PI / 6.0) - PI / 2.0;
        let (inner, color) = if i % 3 == 0 {
            (radius * 0.76, ink)
        } else {
            (radius * 0.88, ink_dim)
        };
        reference_line(
            &mut pixmap,
            cx + angle.cos() * inner,
            cy + angle.sin() * inner,
            cx + angle.cos() * radius,
            cy + angle.sin() * radius,
            color,
            1.0,
        );
    }
    let hour_angle = ((hour % 12) as f32 + minute as f32 / 60.0) * (PI / 6.0) - PI / 2.0;
    let minute_angle = (minute as f32 + second as f32 / 60.0) * (PI / 30.0) - PI / 2.0;
    let second_angle = second as f32 * (PI / 30.0) - PI / 2.0;
    reference_line(
        &mut pixmap,
        cx,
        cy,
        cx + hour_angle.cos() * radius * 0.5,
        cy + hour_angle.sin() * radius * 0.5,
        ink,
        2.2,
    );
    reference_line(
        &mut pixmap,
        cx,
        cy,
        cx + minute_angle.cos() * radius * 0.75,
        cy + minute_angle.sin() * radius * 0.75,
        ink,
        1.4,
    );
    reference_line(
        &mut pixmap,
        cx,
        cy,
        cx + second_angle.cos() * radius * 0.85,
        cy + second_angle.sin() * radius * 0.85,
        Color::rgb(0xB0, 0x30, 0x30),
        0.8,
    );
    pixmap.take()
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn representative_clock_tiles_have_stable_pixels() {
    let theme = nextstep_classic();
    let cases = [
        (16, 0, 0, 0, 0x593e_f5cc_7871_b1c4),
        (56, 3, 15, 0, 0xad69_7835_0861_03a8),
        (64, 10, 9, 30, 0x082e_fd78_4d8f_b03a),
    ];
    let actual: Vec<_> = cases
        .iter()
        .map(|&(size, hour, minute, second, _)| {
            let buffer = render_clock_tile(&theme, size, hour, minute, second);
            let hash = fnv1a(&buffer.pixels);
            eprintln!("clock {size}px {hour:02}:{minute:02}:{second:02}: {hash:#018x}");
            hash
        })
        .collect();
    assert_eq!(actual, cases.map(|case| case.4));
}

#[test]
fn clock_render_reports_its_allocation_budget() {
    let theme = nextstep_classic();
    // Warm lazy dependencies before opening the exact measurement scope.
    std::hint::black_box(render_clock_tile(&theme, 56, 3, 15, 0));

    let (buffer, stats) = measure(|| render_clock_tile(&theme, 56, 3, 15, 0));
    std::hint::black_box(buffer);
    eprintln!("56px clock render: {stats:?}");
    assert_eq!(
        stats.calls, 32,
        "allocation regression in the normal-size clock path"
    );
    assert_eq!(stats.requested_bytes, 24_002);
}

#[test]
fn optimized_clock_matches_the_reference_renderer() {
    let theme = nextstep_classic();
    let mut mismatched_sizes = Vec::new();
    for size in 8..=128 {
        let actual = render_clock_tile(&theme, size, 7, 23, 41);
        let expected = reference_clock_pixels(&theme, size, 7, 23, 41);
        if actual.pixels != expected {
            mismatched_sizes.push(size);
        }
    }
    eprintln!("mismatched sizes: {mismatched_sizes:?}");
    assert!(mismatched_sizes.is_empty());
}
