//! The three workspace styles and their small, deterministic geometry solver.
//! All solver coordinates are output-local logical units, including the gap.
use crate::{ClientId, Point, Rect, Size};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LayoutMode {
    #[default]
    Freeform,
    Mosaic,
    Flow,
}

impl LayoutMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Freeform => "Freeform",
            Self::Mosaic => "Mosaic",
            Self::Flow => "Flow",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "freeform" => Some(Self::Freeform),
            "mosaic" | "dwindle" => Some(Self::Mosaic),
            "flow" | "scrolling" => Some(Self::Flow),
            _ => None,
        }
    }

    pub fn compatible_name(self) -> &'static str {
        match self {
            Self::Freeform => "freeform",
            Self::Mosaic => "dwindle",
            Self::Flow => "scrolling",
        }
    }
}

/// Fixed counters for development instrumentation; no per-frame logging.
#[derive(Clone, Copy, Debug, Default)]
pub struct LayoutStatistics {
    pub calculations: u64,
    pub calculation_us: u128,
    pub managed_windows: usize,
    pub geometry_changes: u64,
}

/// Intent survives temporary presentation, including edits to a floated tile.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowPlacement {
    pub freeform: Option<Rect>,
    pub floating: bool,
    /// EDID identity where available, otherwise connector name.
    pub output: Option<String>,
    /// Logical frame width. Zero chooses the useful default on first entry.
    pub flow_width: u32,
    /// Horizontal/vertical proportions; zero means the default equal share.
    pub mosaic_weight: [u32; 2],
}

#[derive(Clone, Debug, Default)]
pub(crate) struct WorkspaceLayout {
    pub mode: LayoutMode,
    /// Includes floating and minimized clients so returning is reversible.
    pub order: Vec<ClientId>,
    pub viewports: std::collections::HashMap<String, i32>,
}

/// One interactive resize rollback, discarded on commit.
pub(crate) struct ResizeSnapshot {
    pub workspace: usize,
    pub placements: Vec<(ClientId, [u32; 2], u32)>,
    pub viewports: std::collections::HashMap<String, i32>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Item {
    pub min: Size,
    pub weight: [u32; 2],
    pub width: u32,
}

const GAP: u32 = 6;
const DEFAULT_WEIGHT: u32 = 1000;

fn weight(value: u32) -> u64 {
    (if value == 0 {
        DEFAULT_WEIGHT
    } else {
        value.clamp(1, 1_000_000)
    }) as u64
}

/// Weighted, minimum-constrained partition. Rounding remainder stays inside
/// the last cell. None means the caller must float a constrained exception.
fn partition(length: u32, gap: u32, minima: &[u32], weights: &[u32]) -> Option<Vec<u32>> {
    let available = length.checked_sub(gap.checked_mul(minima.len().saturating_sub(1) as u32)?)?;
    let minimum: u64 = minima.iter().map(|&m| m.max(1) as u64).sum();
    if minimum > available as u64 {
        return None;
    }
    let mut result = vec![0; minima.len()];
    let mut remaining = available as u64;
    let mut total: u64 = weights.iter().map(|&w| weight(w)).sum();
    loop {
        let mut clamped = false;
        for (i, &min) in minima.iter().enumerate() {
            if result[i] == 0 && remaining * weight(weights[i]) / total.max(1) < min.max(1) as u64 {
                result[i] = min.max(1);
                remaining -= result[i] as u64;
                total -= weight(weights[i]);
                clamped = true;
            }
        }
        if !clamped {
            break;
        }
    }
    for (i, value) in result.iter_mut().enumerate() {
        if *value != 0 {
            continue;
        }
        let w = weight(weights[i]);
        *value = (remaining * w / total.max(1)) as u32;
        remaining -= *value as u64;
        total -= w;
    }
    Some(result)
}

fn columns(
    area: Size,
    items: &[Item],
    indices: &[usize],
    count: usize,
    gap: u32,
) -> Option<Vec<Rect>> {
    // These are the same minimum constraints enforced by partition below.
    // Reject impossible columns before calculating weights or allocating
    // their geometry: thin, wide/tall clients can fit in total area while
    // making most column arrangements impossible.
    let mut available_width = area
        .w
        .checked_sub(gap.checked_mul(count.saturating_sub(1) as u32)?)?;
    let mut minima = Vec::with_capacity(count);
    for column in 0..count {
        let slice = &indices[column * indices.len() / count..(column + 1) * indices.len() / count];
        let mut available_height = area
            .h
            .checked_sub(gap.checked_mul(slice.len().saturating_sub(1) as u32)?)?;
        let mut minimum_width = 1;
        for &i in slice {
            minimum_width = minimum_width.max(items[i].min.w);
            available_height = available_height.checked_sub(items[i].min.h.max(1))?;
        }
        available_width = available_width.checked_sub(minimum_width)?;
        minima.push(minimum_width);
    }
    let mut weights = Vec::with_capacity(count);
    for column in 0..count {
        let slice = &indices[column * indices.len() / count..(column + 1) * indices.len() / count];
        weights.push(
            (slice
                .iter()
                .map(|&i| weight(items[i].weight[0]))
                .sum::<u64>()
                / slice.len() as u64) as u32,
        );
    }
    let widths = partition(area.w, gap, &minima, &weights)?;
    let mut result = Vec::with_capacity(indices.len());
    let mut x = 0;
    for (column, width) in widths.into_iter().enumerate() {
        let slice = &indices[column * indices.len() / count..(column + 1) * indices.len() / count];
        minima.clear();
        weights.clear();
        for &i in slice {
            minima.push(items[i].min.h);
            weights.push(items[i].weight[1]);
        }
        let heights = partition(area.h, gap, &minima, &weights)?;
        let mut y = 0;
        for height in heights {
            result.push(Rect {
                pos: Point::new(x as i32, y as i32),
                size: Size::new(width, height),
            });
            y += height + gap;
        }
        x += width + gap;
    }
    Some(result)
}

/// Balanced columns, transposed on portrait outputs. Feasibility takes
/// precedence over aspect preference. Infeasible clients are explicit None
/// entries; their membership and saved freeform geometry are never discarded.
pub(crate) fn mosaic(area: Size, items: &[Item]) -> Vec<Option<Rect>> {
    let portrait = area.h > area.w;
    let area = if portrait {
        Size::new(area.h, area.w)
    } else {
        area
    };
    let transposed: Vec<_> = items
        .iter()
        .map(|item| {
            if portrait {
                Item {
                    min: Size::new(item.min.h, item.min.w),
                    weight: [item.weight[1], item.weight[0]],
                    ..*item
                }
            } else {
                *item
            }
        })
        .collect();
    let mut indices: Vec<_> = transposed
        .iter()
        .enumerate()
        .filter(|(_, i)| i.min.w.max(1) <= area.w && i.min.h.max(1) <= area.h)
        .map(|(i, _)| i)
        .collect();
    let mut result = vec![None; items.len()];
    let demand = |i: usize| {
        let min = transposed[i].min;
        min.w.max(1) as u128 * min.h.max(1) as u128
    };
    let mut minimum_area: u128 = indices.iter().map(|&i| demand(i)).sum();
    let available_area = area.w as u128 * area.h as u128;
    while !indices.is_empty() {
        let n = indices.len();
        let preferred = ((n as f64 * area.w as f64 / area.h.max(1) as f64 / 1.6)
            .sqrt()
            .round() as usize)
            .clamp(if n > 1 { 2 } else { 1 }, n);
        let gap = GAP
            .min(area.w / (n as u32 * 4))
            .min(area.h / (n as u32 * 4));
        // Start at the preferred aspect, then test nearest alternatives.
        // No partition can fit when the clients' minimum areas exceed the
        // output. Skip futile column searches while removing the same largest
        // exceptions the fallback below would select anyway.
        for offset in 0..if minimum_area <= available_area { n } else { 0 } {
            for count in [
                preferred.checked_sub(offset),
                (offset != 0).then_some(preferred + offset),
            ]
            .into_iter()
            .flatten()
            {
                if count == 0 || count > n {
                    continue;
                }
                if let Some(rects) = columns(area, &transposed, &indices, count, gap) {
                    for (&i, rect) in indices.iter().zip(rects) {
                        result[i] = Some(if portrait {
                            Rect {
                                pos: Point::new(rect.pos.y, rect.pos.x),
                                size: Size::new(rect.size.h, rect.size.w),
                            }
                        } else {
                            rect
                        });
                    }
                    return result;
                }
            }
        }
        // One demanding client must not force every other client to float.
        let largest = indices
            .iter()
            .enumerate()
            .max_by_key(|(_, i)| {
                let min = transposed[**i].min;
                min.w.max(1) as u64 * min.h.max(1) as u64
            })
            .map(|(i, _)| i)
            .unwrap();
        minimum_area -= demand(indices.remove(largest));
    }
    result
}

pub(crate) fn flow(
    area: Size,
    items: &[Item],
    focused: Option<usize>,
    viewport: &mut i32,
) -> Vec<Option<Rect>> {
    let mut x = 0i32;
    let mut result = Vec::with_capacity(items.len());
    for item in items {
        if item.min.w > area.w || item.min.h > area.h || area.w == 0 || area.h == 0 {
            result.push(None);
            continue;
        }
        let width = (if item.width == 0 {
            area.w * 2 / 3
        } else {
            item.width
        })
        .clamp(item.min.w.max(1), area.w);
        result.push(Some(Rect {
            pos: Point::new(x, 0),
            size: Size::new(width, area.h),
        }));
        x = x.saturating_add(width as i32).saturating_add(GAP as i32);
    }
    let limit = x
        .saturating_sub(GAP as i32)
        .saturating_sub(area.w as i32)
        .max(0);
    *viewport = (*viewport).clamp(0, limit);
    if let Some(Some(rect)) = focused.and_then(|i| result.get(i)) {
        let margin = (area.w / 12).min(area.w.saturating_sub(rect.size.w) / 2) as i32;
        if rect.pos.x < viewport.saturating_add(margin) {
            *viewport = rect.pos.x.saturating_sub(margin).max(0);
        } else if rect.pos.x as i64 + rect.size.w as i64
            > *viewport as i64 + area.w as i64 - margin as i64
        {
            *viewport = (rect.pos.x as i64 + rect.size.w as i64 - area.w as i64 + margin as i64)
                .clamp(0, limit as i64) as i32;
        }
    }
    for rect in result.iter_mut().flatten() {
        rect.pos.x = rect.pos.x.saturating_sub(*viewport);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item() -> Item {
        Item {
            min: Size::new(1, 1),
            weight: [0, 0],
            width: 0,
        }
    }

    fn cross_strip_items(count: usize, ordering: &str) -> Vec<Item> {
        (0..count)
            .map(|index| {
                let wide = match ordering {
                    "wide-first" => index < count / 2,
                    "tall-first" => index >= count / 2,
                    _ => index % 2 == 0,
                };
                Item {
                    min: if wide {
                        Size::new(999, 1)
                    } else {
                        Size::new(1, 999)
                    },
                    ..item()
                }
            })
            .collect()
    }

    fn placement_digest(result: &[Option<Rect>]) -> u64 {
        // Include placement index, presence and every coordinate: matching
        // counts alone can hide changed exclusion order or rounding.
        let mut digest = 0xcbf29ce484222325_u64;
        for (index, rect) in result.iter().enumerate() {
            let fields = match rect {
                Some(rect) => [
                    index as u64,
                    1,
                    rect.pos.x as u64,
                    rect.pos.y as u64,
                    rect.size.w as u64,
                    rect.size.h as u64,
                ],
                None => [index as u64, 0, 0, 0, 0, 0],
            };
            for field in fields {
                for byte in field.to_le_bytes() {
                    digest = (digest ^ byte as u64).wrapping_mul(0x100000001b3);
                }
            }
        }
        digest
    }

    #[test]
    fn incompatible_cross_strips_preserve_original_placements() {
        // Captured from the preserved release baseline before adding early
        // column rejection. Both grouped orders and interleaving matter:
        // the deterministic exclusion rule leaves different clients tiled.
        let expected = [
            (
                8,
                [0xbbb6f75d69cb2317, 0x1e93da6caff6ac6f, 0x4f6b31a614d94264],
            ),
            (
                32,
                [0xddb091631fd4e901, 0xec5e1cf7391541b1, 0x995ad8b0cbe9ba64],
            ),
            (
                64,
                [0x086adfb526eb9793, 0x45858ad6693acdb3, 0x588656e40beaaa64],
            ),
            (
                128,
                [0xbb44b53a9c92fc43, 0xea12354879ba2113, 0xea38184942581264],
            ),
            (
                256,
                [0x84da81edaa233a4d, 0x56fdfd724a138d45, 0x968557edc3dcda64],
            ),
            (
                512,
                [0x61c9337bf7f4cef1, 0xd9f3ac09e424dab1, 0xdc91532411361f64],
            ),
        ];
        for (count, digests) in expected {
            for (ordering, expected) in ["wide-first", "tall-first", "alternating"]
                .into_iter()
                .zip(digests)
            {
                let items = cross_strip_items(count, ordering);
                let result = mosaic(Size::new(1000, 1000), &items);
                assert_eq!(
                    placement_digest(&result),
                    expected,
                    "{count} clients, {ordering}"
                );
            }
        }
    }

    /// Geometrically incompatible minima can fit in total area, bypassing
    /// the existing area rejection. Retain every sample and the exact output
    /// digest when comparing an optimization with a preserved baseline build.
    #[test]
    #[ignore = "performance profile: cargo test --release -p wm-core mosaic_cross_strips_profile -- --ignored --nocapture"]
    fn mosaic_cross_strips_profile() {
        use std::hint::black_box;
        use std::time::Instant;
        let area = Size::new(1000, 1000);
        for count in [8, 32, 64, 128, 256, 512] {
            for ordering in ["wide-first", "tall-first", "alternating"] {
                let items = cross_strip_items(count, ordering);
                assert!(
                    items
                        .iter()
                        .map(|item| item.min.w as u64 * item.min.h as u64)
                        .sum::<u64>()
                        < area.w as u64 * area.h as u64
                );
                let mut expected = None;
                for sample in 0..3 {
                    let started = Instant::now();
                    let result = mosaic(black_box(area), black_box(&items));
                    let elapsed = started.elapsed();
                    let digest = placement_digest(&result);
                    if let Some(expected) = &expected {
                        assert_eq!(&result, expected);
                    }
                    for (index, rect) in result
                        .iter()
                        .enumerate()
                        .filter_map(|(i, r)| r.map(|r| (i, r)))
                    {
                        assert!(
                            rect.size.w >= items[index].min.w && rect.size.h >= items[index].min.h
                        );
                        assert_eq!(
                            Rect::new(Point::new(0, 0), area).intersection(rect),
                            Some(rect)
                        );
                        assert!(result
                            .iter()
                            .skip(index + 1)
                            .flatten()
                            .all(|other| rect.intersection(*other).is_none()));
                    }
                    println!("mosaic-cross-strips count={count} ordering={ordering} sample={sample} elapsed_us={} included={} digest={digest:016x}",
                        elapsed.as_micros(), result.iter().flatten().count());
                    expected = Some(result);
                }
            }
        }
    }

    #[test]
    fn generated_constraints_and_proportions_never_break_geometry() {
        let mut seed = 0x43484f4e4b535445u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u32
        };
        for case in 0..2000 {
            let area = Size::new(next() % 2001, next() % 2001);
            let items: Vec<_> = (0..1 + next() % 32)
                .map(|_| Item {
                    min: Size::new(next() % 1501, next() % 1501),
                    weight: [next(), next()],
                    width: next(),
                })
                .collect();
            let result = mosaic(area, &items);
            assert_eq!(result, mosaic(area, &items), "case {case}");
            for (i, rect) in result
                .iter()
                .enumerate()
                .filter_map(|(i, r)| r.map(|r| (i, r)))
            {
                assert!(rect.size.w >= items[i].min.w.max(1));
                assert!(rect.size.h >= items[i].min.h.max(1));
                assert_eq!(
                    Rect::new(Point::new(0, 0), area).intersection(rect),
                    Some(rect)
                );
                for other in result.iter().skip(i + 1).flatten() {
                    assert!(rect.intersection(*other).is_none(), "case {case}");
                }
            }
            let mut viewport = next() as i32;
            let focused = next() as usize % items.len();
            let flow = flow(area, &items, Some(focused), &mut viewport);
            if let Some(rect) = flow[focused] {
                assert!(rect.pos.x >= 0 && rect.pos.x as u32 + rect.size.w <= area.w);
            }
            assert!(viewport >= 0);
            let mut previous = None;
            for rect in flow.iter().flatten() {
                assert!(rect.size.w > 0 && rect.size.h > 0);
                if let Some(before) = previous {
                    assert!(rect.pos.x > before);
                }
                previous = Some(rect.pos.x + rect.size.w as i32);
            }
        }
    }

    #[test]
    #[ignore = "development solver profile; product-path benchmark lives in chonk-testkit"]
    fn constrained_solver_profile() {
        for n in [8, 64, 256] {
            let items = vec![
                Item {
                    min: Size::new(600, 600),
                    ..item()
                };
                n
            ];
            let started = std::time::Instant::now();
            let output = mosaic(Size::new(1000, 1000), std::hint::black_box(&items));
            eprintln!(
                "{n} demanding clients: {} us; {} managed",
                started.elapsed().as_micros(),
                output.iter().flatten().count()
            );
            assert_eq!(output.iter().flatten().count(), 1);
        }
    }

    #[test]
    fn mosaic_is_deterministic_positive_disjoint_and_contained() {
        for area in [
            Size::new(1920, 1080),
            Size::new(801, 1399),
            Size::new(17, 13),
            Size::new(1, 1),
            Size::new(0, 0),
        ] {
            for n in 1..=10 {
                let items = vec![item(); n];
                let rects = mosaic(area, &items);
                assert_eq!(rects, mosaic(area, &items));
                if area.w as u64 * area.h as u64 >= n as u64 {
                    assert!(rects.iter().all(Option::is_some));
                }
                for (i, a) in rects
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| r.map(|r| (i, r)))
                {
                    assert!(a.size.w > 0 && a.size.h > 0);
                    assert!(a.pos.x >= 0 && a.pos.y >= 0);
                    assert!(
                        a.pos.x as u32 + a.size.w <= area.w && a.pos.y as u32 + a.size.h <= area.h
                    );
                    for b in rects.iter().skip(i + 1).flatten() {
                        assert!(
                            a.pos.x + a.size.w as i32 <= b.pos.x
                                || b.pos.x + b.size.w as i32 <= a.pos.x
                                || a.pos.y + a.size.h as i32 <= b.pos.y
                                || b.pos.y + b.size.h as i32 <= a.pos.y
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn three_windows_have_a_large_first_region_and_portrait_transposes() {
        let items = vec![item(); 3];
        let r = mosaic(Size::new(1200, 800), &items);
        assert_eq!(r[0].unwrap().size.h, 800);
        assert_eq!(r[1].unwrap().pos.x, r[2].unwrap().pos.x);
        let portrait = mosaic(Size::new(800, 1200), &items);
        assert_eq!(portrait[0].unwrap().size.w, 800);
    }

    #[test]
    fn minima_are_respected_and_a_bad_client_does_not_break_its_neighbors() {
        let items = [
            Item {
                min: Size::new(900, 200),
                ..item()
            },
            item(),
            Item {
                min: Size::new(u32::MAX, u32::MAX),
                ..item()
            },
        ];
        let r = mosaic(Size::new(1000, 800), &items);
        assert!(r[0].unwrap().size.w >= 900 && r[1].is_some() && r[2].is_none());
        assert_eq!(partition(101, 1, &[80, 1], &[1, 1000]), Some(vec![80, 20]));
    }

    #[test]
    fn flow_follows_focus_and_retains_distinct_widths() {
        let items = [
            Item {
                width: 400,
                ..item()
            },
            Item {
                width: 700,
                ..item()
            },
            item(),
        ];
        let mut viewport = 0;
        let r = flow(Size::new(1000, 700), &items, Some(2), &mut viewport);
        assert!(viewport > 0);
        assert_eq!(r[0].unwrap().size.w, 400);
        assert_eq!(r[1].unwrap().size.w, 700);
        let last = r[2].unwrap();
        assert!(last.pos.x >= 0 && last.pos.x as u32 + last.size.w <= 1000);
        flow(Size::new(1000, 700), &items, Some(0), &mut viewport);
        assert_eq!(viewport, 0);
    }
}
