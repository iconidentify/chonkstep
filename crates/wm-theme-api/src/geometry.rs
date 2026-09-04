#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Size {
    pub w: u32,
    pub h: u32,
}

impl Size {
    pub const fn new(w: u32, h: u32) -> Self {
        Self { w, h }
    }
}

/// Absolute ceiling for one client window dimension in physical
/// pixels. The effective limit is normally the desktop's own extent
/// (see [`client_size_limit`]); this second guard keeps a malformed
/// multi-output layout or an unattached protocol surface from turning
/// a client-controlled size into a multi-gigabyte decoration buffer.
pub const MAX_CLIENT_WINDOW_DIMENSION: u32 = 8192;

/// Largest useful client size on this desktop.
///
/// A managed window cannot expose pixels beyond the union of the
/// outputs, so accepting a larger content rectangle only increases
/// allocations and protocol work. Each axis is independently bounded
/// by the real desktop and by [`MAX_CLIENT_WINDOW_DIMENSION`]. A
/// temporarily empty output layout still permits a 1x1 safe fallback.
pub const fn client_size_limit(screen: Size) -> Size {
    Size::new(
        if screen.w == 0 {
            1
        } else if screen.w > MAX_CLIENT_WINDOW_DIMENSION {
            MAX_CLIENT_WINDOW_DIMENSION
        } else {
            screen.w
        },
        if screen.h == 0 {
            1
        } else if screen.h > MAX_CLIENT_WINDOW_DIMENSION {
            MAX_CLIENT_WINDOW_DIMENSION
        } else {
            screen.h
        },
    )
}

/// Bounds an untrusted client size before it can reach layout or
/// raster allocation. Zero is preserved because protocol-specific
/// callers use it to mean "unspecified"; positive dimensions are
/// capped to the useful desktop limit.
pub const fn clamp_client_size(size: Size, screen: Size) -> Size {
    let limit = client_size_limit(screen);
    Size::new(
        if size.w > limit.w { limit.w } else { size.w },
        if size.h > limit.h { limit.h } else { size.h },
    )
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub pos: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(pos: Point, size: Size) -> Self {
        Self { pos, size }
    }

    /// Whether `p` (in the same coordinate space as this rect) falls
    /// within it. Half-open: the far edge is excluded.
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.pos.x
            && p.y >= self.pos.y
            && p.x < self.pos.x + self.size.w as i32
            && p.y < self.pos.y + self.size.h as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_contains_is_half_open() {
        let r = Rect::new(Point::new(10, 10), Size::new(5, 5));
        assert!(r.contains(Point::new(10, 10)));
        assert!(r.contains(Point::new(14, 14)));
        assert!(!r.contains(Point::new(15, 15)));
        assert!(!r.contains(Point::new(9, 10)));
    }

    #[test]
    fn client_size_is_bounded_by_the_desktop_and_absolute_ceiling() {
        assert_eq!(
            clamp_client_size(Size::new(u32::MAX, 100_000), Size::new(2560, 1600)),
            Size::new(2560, 1600)
        );
        assert_eq!(
            client_size_limit(Size::new(20_000, 10_000)),
            Size::new(MAX_CLIENT_WINDOW_DIMENSION, MAX_CLIENT_WINDOW_DIMENSION)
        );
        assert_eq!(
            clamp_client_size(Size::new(640, 480), Size::new(2560, 1600)),
            Size::new(640, 480)
        );
    }
}
