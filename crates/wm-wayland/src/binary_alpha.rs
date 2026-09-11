//! Skip zero-alpha texels in pixel-aligned, binary-alpha chrome. Drawing a
//! transparent corner still submits a blended GPU draw; on llvmpipe that tiny
//! draw also schedules raster workers. Keep the original element's damage,
//! opacity and identity, and batch only visible pixels in its draw call.
use smithay::backend::renderer::{
    element::{Element, Id, Kind, RenderElement, UnderlyingStorage},
    utils::{CommitCounter, DamageSet, OpaqueRegions}, Renderer,
};
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};

type PixelRect = Rectangle<i32, Physical>;

#[derive(Debug)]
/// The caller must establish that every source texel is opaque or RGBA zero.
pub(crate) struct BinaryAlpha<E>(pub E);

impl<E: Element> Element for BinaryAlpha<E> {
    fn id(&self) -> &Id { self.0.id() }
    fn current_commit(&self) -> CommitCounter { self.0.current_commit() }
    fn transform(&self) -> Transform { self.0.transform() }
    fn src(&self) -> Rectangle<f64, Buffer> { self.0.src() }
    fn geometry(&self, scale: Scale<f64>) -> PixelRect { self.0.geometry(scale) }
    fn damage_since(&self, scale: Scale<f64>, commit: Option<CommitCounter>) -> DamageSet<i32, Physical> {
        self.0.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> { self.0.opaque_regions(scale) }
    fn alpha(&self) -> f32 { self.0.alpha() }
    fn kind(&self) -> Kind { self.0.kind() }
}

impl<R: Renderer, E: RenderElement<R>> RenderElement<R> for BinaryAlpha<E> {
    fn draw(&self, frame: &mut R::Frame<'_, '_>, src: Rectangle<f64, Buffer>, dst: PixelRect,
        damage: &[PixelRect], opaque: &[PixelRect]) -> Result<(), R::Error> {
        // Interpolation can produce partial alpha at an edge. Only the normal
        // 1:1 integer-pixel path may omit everything outside the opaque mask.
        // A fade, transformed texture, or unusually fragmented damage keeps
        // the ordinary renderer's behavior, without allocating a scratch Vec.
        if self.alpha() == 1.0 && self.transform() == Transform::Normal && !opaque.is_empty()
            && src.size.w == f64::from(dst.size.w) && src.size.h == f64::from(dst.size.h)
            && src.loc.x.fract() == 0.0 && src.loc.y.fract() == 0.0 {
            let mut pieces = [PixelRect::default(); 32];
            if let Some(len) = visible_damage(damage, opaque, &mut pieces) {
                if len == 0 { return Ok(()); }
                return self.0.draw(frame, src, dst, &pieces[..len], opaque);
            }
        }
        self.0.draw(frame, src, dst, damage, opaque)
    }

    fn underlying_storage(&self, renderer: &mut R) -> Option<UnderlyingStorage<'_>> {
        self.0.underlying_storage(renderer)
    }
}

fn visible_damage(damage: &[PixelRect], opaque: &[PixelRect], out: &mut [PixelRect]) -> Option<usize> {
    let mut len = 0;
    for damaged in damage {
        for solid in opaque {
            if let Some(rect) = damaged.intersection(*solid) {
                *out.get_mut(len)? = rect;
                len += 1;
            }
        }
    }
    Some(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_holes_are_omitted_without_losing_any_damaged_opaque_pixel() {
        let rect = |x, y, w, h| PixelRect::new((x, y).into(), (w, h).into());
        let opaque = [rect(0, 0, 19, 1), rect(0, 1, 20, 8)];
        for damage in [[rect(0, 0, 20, 9), rect(0, 0, 0, 0)],
            [rect(18, 0, 2, 2), rect(2, 5, 9, 4)],
            [rect(19, 0, 1, 1), rect(0, 0, 0, 0)]] {
            let mut out = [PixelRect::default(); 32];
            let count = visible_damage(&damage, &opaque, &mut out).unwrap();
            for y in 0..9 {
                for x in 0..20 {
                    let point = (x, y);
                    assert_eq!(out[..count].iter().any(|r| r.contains(point)),
                        damage.iter().any(|r| r.contains(point)) && opaque.iter().any(|r| r.contains(point)));
                }
            }
        }
        assert_eq!(visible_damage(&[rect(0, 0, 20, 9)], &opaque, &mut [PixelRect::default(); 1]), None,
            "fragmentation falls back instead of allocating or silently truncating damage");
    }
}
