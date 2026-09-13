//! Bounded visual effects, separate from frame/input geometry and raster storage.
use crate::{Point, Rect, Size};

fn bounded_rect(mut rect: Rect) -> Rect {
    rect.size.w = rect.size.w.min(16384);
    rect.size.h = rect.size.h.min(16384);
    // Keep enough signed-coordinate headroom for the largest allowed blur,
    // offset, inset, and backend edge arithmetic. Invalid distant rectangles
    // remain offscreen; normalization never turns them into giant canvases.
    rect.pos.x = rect
        .pos
        .x
        .clamp(i32::MIN + 2048, i32::MAX - 2048 - rect.size.w as i32);
    rect.pos.y = rect
        .pos
        .y
        .clamp(i32::MIN + 2048, i32::MAX - 2048 - rect.size.h as i32);
    rect
}

/// A rounded visual outline. `rect` is frame-local; its radius is device pixels.
/// Backends apply it to client content and chrome, including input, while
/// preserving the separately declared invisible resize margin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationShape {
    pub rect: Rect,
    pub radius: u16,
    pub border: u16,
    pub border_rgb: [u8; 3],
}

impl DecorationShape {
    pub fn normalized(mut self) -> Self {
        self.rect = bounded_rect(self.rect);
        self.radius = self
            .radius
            .min(256)
            .min((self.rect.size.w / 2).min(self.rect.size.h / 2).min(256) as u16);
        self.border = self.border.min(16).min(self.radius);
        self
    }

    /// Pixel-center coverage, shared by binary-input and software backends.
    pub fn coverage(self, at: Point) -> f32 {
        let shape = self.normalized();
        if !shape.rect.contains(at) {
            return 0.0;
        }
        let r = f32::from(shape.radius);
        if r == 0.0 {
            return 1.0;
        }
        let x = (i64::from(at.x) - i64::from(shape.rect.pos.x)) as f32 + 0.5;
        let y = (i64::from(at.y) - i64::from(shape.rect.pos.y)) as f32 + 0.5;
        let dx = (r - x).max(x - (shape.rect.size.w as f32 - r)).max(0.0);
        let dy = (r - y).max(y - (shape.rect.size.h as f32 - r)).max(0.0);
        (r + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0)
    }
}

/// CSS-style outer shadow: blur is the CSS blur diameter (Gaussian sigma is
/// half that value). It never enlarges input geometry or a retained RGBA image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecorationShadow {
    pub rect: Rect,
    pub radius: u16,
    pub offset: Point,
    pub blur: u16,
    pub rgba: [u8; 4],
}

impl DecorationShadow {
    pub fn normalized(mut self) -> Self {
        self.rect = bounded_rect(self.rect);
        self.radius = self
            .radius
            .min(256)
            .min((self.rect.size.w / 2).min(self.rect.size.h / 2).min(256) as u16);
        self.blur = self.blur.min(512);
        self.offset.x = self.offset.x.clamp(-512, 512);
        self.offset.y = self.offset.y.clamp(-512, 512);
        self
    }

    /// A finite 3-sigma effect extent. Wide arithmetic also bounds malicious
    /// descriptors near either i32 edge before conversion to backend geometry.
    pub fn effect_bounds(self) -> Rect {
        let shadow = self.normalized();
        let spread = i64::from(shadow.blur).saturating_mul(3).div_euclid(2);
        let left = (i64::from(shadow.rect.pos.x) + i64::from(shadow.offset.x) - spread)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        let top = (i64::from(shadow.rect.pos.y) + i64::from(shadow.offset.y) - spread)
            .clamp(i64::from(i32::MIN), i64::from(i32::MAX));
        let right = (i64::from(shadow.rect.pos.x)
            + i64::from(shadow.offset.x)
            + i64::from(shadow.rect.size.w)
            + spread)
            .clamp(left, i64::from(i32::MAX));
        let bottom = (i64::from(shadow.rect.pos.y)
            + i64::from(shadow.offset.y)
            + i64::from(shadow.rect.size.h)
            + spread)
            .clamp(top, i64::from(i32::MAX));
        Rect::new(
            Point::new(left as i32, top as i32),
            Size::new((right - left) as u32, (bottom - top) as u32),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effect_bounds_are_separate_and_bounded() {
        let rect = Rect::new(Point::new(5, 5), Size::new(802, 635));
        let shadow = DecorationShadow {
            rect,
            radius: 9,
            offset: Point::new(0, 10),
            blur: 30,
            rgba: [6, 9, 22, 112],
        };
        assert_eq!(
            shadow.effect_bounds(),
            Rect::new(Point::new(-40, -30), Size::new(892, 725))
        );
        assert_eq!(shadow.rect, rect);
        for pos in [i32::MIN, i32::MAX] {
            let bounded = DecorationShadow {
                rect: Rect::new(Point::new(pos, pos), Size::new(u32::MAX, u32::MAX)),
                radius: u16::MAX,
                offset: Point::new(i32::MAX, i32::MIN),
                blur: u16::MAX,
                rgba: [0; 4],
            }
            .normalized();
            assert_eq!(bounded.blur, 512);
            let _ = bounded.effect_bounds();
        }
    }
}
