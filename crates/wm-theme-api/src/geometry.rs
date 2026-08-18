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
}
