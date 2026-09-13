//! A shared bounded Gaussian atlas. Moving/resizing a window only changes eight
//! source/destination rectangles; the expensive convolution runs on cache miss.
use std::cell::RefCell;

use smithay::backend::{
    allocator::Fourcc,
    renderer::{
        ContextId, ImportMem, Renderer,
        gles::{GlesError, GlesFrame, GlesRenderer, GlesTexture, ffi},
    },
};
use wm_theme_api::DecorationShadow;

const CAPACITY: usize = 8;
const MAX_SIDE: u32 = 256;

#[derive(Debug, Clone)]
pub(crate) struct Atlas {
    texture: GlesTexture,
    context: ContextId<GlesTexture>,
    pub inset: u32,
    pub pad: u32,
    pub side: u32,
    pub pixels: u32,
}

impl Atlas {
    /// This texture is fully uploaded before publication and is never mutated.
    /// Compatible contexts may read it without per-draw texture read fences;
    /// its Arc stays alive through the complete draw and normal GL deletion.
    pub fn with_bound<R>(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        draw: impl FnOnce(&mut GlesFrame<'_, '_>) -> Result<R, GlesError>,
    ) -> Result<R, GlesError> {
        if frame
            .egl_context()
            .user_data()
            .get::<ContextId<GlesTexture>>()
            != Some(&self.context)
        {
            return Err(GlesError::MappingError);
        }
        // SAFETY: The frame owns the current GL context, the query outputs are valid
        // i32 slots, and the ContextId check above verifies the retained texture
        // belongs to this share group. Its owner remains alive through the draw.
        let previous = frame.with_context(|gl| unsafe {
            let (mut active, mut binding) = (0, 0);
            gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active);
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut binding);
            gl.BindTexture(ffi::TEXTURE_2D, self.texture.tex_id());
            (active, binding)
        })?;
        let result = draw(frame);
        // SAFETY: Restore the saved texture unit and binding in the same active
        // frame context; these calls do not dereference application pointers.
        frame.with_context(|gl| unsafe {
            gl.ActiveTexture(ffi::TEXTURE0);
            gl.BindTexture(ffi::TEXTURE_2D, previous.1 as u32);
            gl.ActiveTexture(previous.0 as u32);
        })?;
        result
    }

    #[cfg(test)]
    pub fn texture(&self) -> &GlesTexture {
        &self.texture
    }
}

#[derive(Debug, Default)]
struct Cache(RefCell<Vec<CacheEntry>>);

type CacheEntry = ((u16, u16, [u8; 4]), Option<Atlas>);

pub(crate) fn atlas(renderer: &mut GlesRenderer, shadow: DecorationShadow) -> Option<Atlas> {
    let key = (shadow.radius, shadow.blur, shadow.rgba);
    let pad = u32::from(shadow.blur) * 3 / 2;
    let inset = u32::from(shadow.radius) + pad;
    // A nine-slice center must lie entirely inside the unshifted opaque frame.
    // Tiny frames and unusually displaced shadows retain the bounded analytic
    // path, where convolution of opposite edges cannot be separated.
    if shadow.blur == 0
        || shadow.offset.x.unsigned_abs() > pad
        || shadow.offset.y.unsigned_abs() > pad
        || shadow.rect.size.w <= 2 * inset
        || shadow.rect.size.h <= 2 * inset
    {
        return None;
    }
    let side = 2 * (inset + pad) + 1;
    let pixels = side.min(MAX_SIDE);
    // A capped atlas must still resolve the blur. A thin authored shadow on a
    // very large corner keeps the analytic path rather than aliasing its edge.
    if f64::from(shadow.blur) * 0.5 < 2.0 * f64::from(side) / f64::from(pixels) {
        return None;
    }
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(Cache::default);
    {
        let cache = renderer.egl_context().user_data().get::<Cache>().unwrap();
        let mut entries = cache.0.borrow_mut();
        if let Some(index) = entries.iter().position(|(found, _)| *found == key) {
            let entry = entries.remove(index);
            let result = entry.1.clone();
            entries.push(entry);
            return result;
        }
    }
    let mut data = raster(shadow.radius, shadow.blur, pixels);
    tint(&mut data, shadow.rgba);
    let texture = renderer
        .import_memory(
            &data,
            Fourcc::Abgr8888,
            (pixels as i32, pixels as i32).into(),
            false,
        )
        .map_err(|error| tracing::warn!(?error, "Gaussian shadow atlas unavailable"))
        .ok();
    // CPU pixels are dropped immediately. The cache owns at most eight 256²
    // RGBA textures (2 MiB), shared by windows/outputs/scene walkers. An evicted
    // texture remains alive only while an already-built scene references it.
    let atlas = texture.and_then(|texture| {
        // Set immutable sampling state once, then complete the upload before
        // any cache reader can observe it. Completion is a cold cache-miss
        // cost; it also makes later reads safe in a compatible shared context
        // without relying on EGL raw-handle identity or owner lifetime.
        renderer
            // SAFETY: with_context makes this renderer current. The owned imported
            // texture and query output slots stay valid throughout the callback;
            // sampling state is set before publication and GL state is restored.
            .with_context(|gl| unsafe {
                let (mut active, mut binding) = (0, 0);
                gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active);
                gl.ActiveTexture(ffi::TEXTURE0);
                gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut binding);
                gl.BindTexture(ffi::TEXTURE_2D, texture.tex_id());
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MIN_FILTER, ffi::LINEAR as i32);
                gl.TexParameteri(ffi::TEXTURE_2D, ffi::TEXTURE_MAG_FILTER, ffi::LINEAR as i32);
                gl.BindTexture(ffi::TEXTURE_2D, binding as u32);
                gl.ActiveTexture(active as u32);
                gl.Finish();
            })
            .ok()?;
        Some(Atlas {
            texture,
            context: renderer.context_id(),
            inset,
            pad,
            side,
            pixels,
        })
    });
    let cache = renderer.egl_context().user_data().get::<Cache>().unwrap();
    let mut entries = cache.0.borrow_mut();
    if entries.len() == CAPACITY {
        entries.remove(0);
    }
    entries.push((key, atlas.clone()));
    atlas
}

/// Premultiply directly from the unquantized product, so tinting adds at most
/// half a channel value of quantization before later interpolation/masking.
fn tint(data: &mut [u8], rgba: [u8; 4]) {
    for pixel in data.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]) * u32::from(rgba[3]);
        for channel in 0..3 {
            pixel[channel] = ((alpha * u32::from(rgba[channel]) + 32_512) / 65_025) as u8;
        }
        pixel[3] = ((alpha + 127) / 255) as u8;
    }
}

fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    x.signum()
        * (1.0
            - (((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t
                + 0.254829592)
                * t)
                * (-x * x).exp())
}

/// Same Gaussian definition as the analytic fallback, with finer quadrature
/// computed once. Atlas samples are linear-filtered during fractional scaling.
#[cfg(test)]
pub(crate) fn gaussian(x: f64, y: f64, w: f64, h: f64, radius: f64, sigma: f64) -> f64 {
    let integral = |lo: f64, hi: f64| {
        0.5 * (erf(hi / (sigma * std::f64::consts::SQRT_2))
            - erf(lo / (sigma * std::f64::consts::SQRT_2)))
    };
    let x = x - w * 0.5;
    let y = y - h * 0.5;
    if radius == 0.0 {
        return integral(-w * 0.5 - x, w * 0.5 - x) * integral(-h * 0.5 - y, h * 0.5 - y);
    }
    let lo = (-h * 0.5).max(y - 3.0 * sigma);
    let hi = (h * 0.5).min(y + 3.0 * sigma);
    let step = (hi - lo).max(0.0) / 32.0;
    let mut value = 0.0;
    for sample in 0..32 {
        let at = lo + (f64::from(sample) + 0.5) * step;
        let dy = (at.abs() - (h * 0.5 - radius)).max(0.0);
        let reach = w * 0.5 - radius + (radius * radius - dy * dy).max(0.0).sqrt();
        let n = (at - y) / sigma;
        value += integral(-reach - x, reach - x) * (-0.5 * n * n).exp() * step
            / (sigma * (2.0 * std::f64::consts::PI).sqrt());
    }
    value.clamp(0.0, 1.0)
}

fn raster(radius: u16, blur: u16, pixels: u32) -> Vec<u8> {
    let pad = f64::from(u32::from(blur) * 3 / 2);
    let radius = f64::from(radius);
    let inset = radius + pad;
    let rect = inset * 2.0 + 1.0;
    let side = rect + 2.0 * pad;
    let step = side / f64::from(pixels);
    let sigma = f64::from(blur) * 0.5;
    let sigma_root = sigma * std::f64::consts::SQRT_2;
    // Integrate every horizontal source span analytically, then convolve those
    // rows with one precomputed vertical kernel. This moves exp/erf work out of
    // the per-pixel 32-sample loop and keeps cold theme selection below a frame.
    let mut rows = vec![0.0_f32; pixels as usize * pixels as usize];
    for y in 0..pixels {
        let low = (f64::from(y) * step - pad).max(0.0);
        let high = ((f64::from(y) + 1.0) * step - pad).min(rect);
        if high <= low {
            continue;
        }
        let coverage = (high - low) / step;
        let at = (low + high) * 0.5 - rect * 0.5;
        let dy = (at.abs() - (rect * 0.5 - radius)).max(0.0);
        let reach = rect * 0.5 - radius + (radius * radius - dy * dy).max(0.0).sqrt();
        for x in 0..pixels {
            let at = (f64::from(x) + 0.5) * step - pad - rect * 0.5;
            rows[(y * pixels + x) as usize] = (coverage
                * 0.5
                * (erf((reach - at) / sigma_root) - erf((-reach - at) / sigma_root)))
                as f32;
        }
    }
    let reach = (3.0 * sigma / step).ceil() as usize;
    let weights = (0..=reach)
        .map(|distance| {
            let d = distance as f64 * step;
            (0.5 * (erf((d + step * 0.5) / sigma_root) - erf((d - step * 0.5) / sigma_root))) as f32
        })
        .collect::<Vec<_>>();
    let mut filtered = vec![0.0_f32; pixels as usize * pixels as usize];
    for y in 0..pixels as usize {
        let output = &mut filtered[y * pixels as usize..(y + 1) * pixels as usize];
        for sy in y.saturating_sub(reach)..=(y + reach).min(pixels as usize - 1) {
            let weight = weights[y.abs_diff(sy)];
            let input = &rows[sy * pixels as usize..(sy + 1) * pixels as usize];
            for (out, &sample) in output.iter_mut().zip(input) {
                *out += sample * weight;
            }
        }
    }
    let mut data = vec![0; pixels as usize * pixels as usize * 4];
    for (out, value) in data.as_chunks_mut::<4>().0.iter_mut().zip(filtered) {
        out.fill((value * 255.0).round().clamp(0.0, 255.0) as u8);
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "requires GLES/EGL; checks immutable upload, shared contexts and GL state"]
    fn native_immutable_atlas_shared_context_eviction_and_state_restoration() {
        use smithay::backend::{
            egl::{EGLContext, EGLDisplay, native::EGLSurfacelessDisplay},
            renderer::{
                Bind, Color32F, ExportMem, Frame, Offscreen,
                gles::{Uniform, UniformName, UniformType},
            },
        };
        use smithay::utils::{Rectangle, Transform};
        use wm_theme_api::{Point, Rect, Size};
        // SAFETY: Only Smithay creates and terminates this test's surfaceless EGL display.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let origin = EGLContext::new(&display).unwrap();
        // Create the sharing context before upload; upload completion must
        // suffice even though this context never issued the producer commands.
        let shared = EGLContext::new_shared(&display, &origin).unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut producer = unsafe { GlesRenderer::new(origin) }.unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut consumer = unsafe { GlesRenderer::new(shared) }.unwrap();
        let shadow = DecorationShadow {
            rect: Rect::new(Point::new(60, 50), Size::new(800, 600)),
            radius: 9,
            blur: 30,
            offset: Point::new(0, 10),
            rgba: [6, 9, 22, 112],
        };
        let retained = atlas(&mut producer, shadow).unwrap();
        let reused = atlas(&mut consumer, shadow).unwrap();
        assert_eq!(retained.texture.tex_id(), reused.texture.tex_id());
        // Eviction must not destroy a texture referenced by an existing scene.
        for radius in 0..9 {
            let mut next = shadow;
            next.radius = radius;
            assert!(atlas(&mut producer, next).is_some());
        }
        let cache = producer.egl_context().user_data().get::<Cache>().unwrap();
        assert_eq!(cache.0.borrow().len(), CAPACITY);
        assert!(
            !cache
                .0
                .borrow()
                .iter()
                .any(|(key, _)| *key == (9, 30, shadow.rgba))
        );
        let program = consumer
            .compile_custom_pixel_shader(
                r#"
            precision highp float; varying vec2 v_coords; uniform sampler2D atlas;
            void main(){gl_FragColor=texture2D(atlas,v_coords);}
        "#,
                &[UniformName::new("atlas", UniformType::_1i)],
            )
            .unwrap();
        let size = (retained.pixels as i32, retained.pixels as i32);
        let mut texture: GlesTexture = consumer
            .create_buffer(Fourcc::Abgr8888, size.into())
            .unwrap();
        let mut target = consumer.bind(&mut texture).unwrap();
        let mut frame = consumer
            .render(&mut target, size.into(), Transform::Normal)
            .unwrap();
        let full = Rectangle::from_size(size.into());
        frame.clear(Color32F::TRANSPARENT, &[full]).unwrap();
        let sentinels = frame
            // SAFETY: The active frame supplies the context, and names has two
            // writable entries for GenTextures. Both names remain owned below.
            .with_context(|gl| unsafe {
                let mut names = [0; 2];
                gl.GenTextures(2, names.as_mut_ptr());
                gl.ActiveTexture(ffi::TEXTURE0);
                gl.BindTexture(ffi::TEXTURE_2D, names[0]);
                gl.ActiveTexture(ffi::TEXTURE1);
                gl.BindTexture(ffi::TEXTURE_2D, names[1]);
                names
            })
            .unwrap();
        let check_state = |frame: &mut GlesFrame<'_, '_>| {
            frame
                // SAFETY: Query valid i32 slots in the active frame context. The
                // sentinel textures are still live and texture-unit state is restored.
                .with_context(|gl| unsafe {
                    let (mut active, mut binding) = (0, 0);
                    gl.GetIntegerv(ffi::ACTIVE_TEXTURE, &mut active);
                    assert_eq!(active, ffi::TEXTURE1 as i32);
                    gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut binding);
                    assert_eq!(binding, sentinels[1] as i32);
                    gl.ActiveTexture(ffi::TEXTURE0);
                    gl.GetIntegerv(ffi::TEXTURE_BINDING_2D, &mut binding);
                    assert_eq!(binding, sentinels[0] as i32);
                    gl.ActiveTexture(ffi::TEXTURE1);
                })
                .unwrap();
        };
        retained
            .with_bound(&mut frame, |frame| {
                frame.render_pixel_shader_to(
                    &program,
                    Rectangle::from_size((f64::from(size.0), f64::from(size.1)).into()),
                    full,
                    size.into(),
                    Some(&[full]),
                    1.0,
                    &[Uniform::new("atlas", 0_i32)],
                )
            })
            .unwrap();
        check_state(&mut frame);
        let failed: Result<(), _> =
            retained.with_bound(&mut frame, |_| Err(GlesError::UnexpectedSize));
        assert!(matches!(failed, Err(GlesError::UnexpectedSize)));
        check_state(&mut frame);
        frame
            // SAFETY: Both sentinel names were generated in this active context,
            // remain owned by this test, and are unbound before deletion. The
            // two-entry input array stays valid throughout DeleteTextures.
            .with_context(|gl| unsafe {
                gl.ActiveTexture(ffi::TEXTURE0);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
                gl.ActiveTexture(ffi::TEXTURE1);
                gl.BindTexture(ffi::TEXTURE_2D, 0);
                gl.DeleteTextures(2, sentinels.as_ptr());
                gl.ActiveTexture(ffi::TEXTURE0);
            })
            .unwrap();
        frame.finish().unwrap().wait().unwrap();
        let mapping = consumer
            .copy_framebuffer(&target, Rectangle::from_size(size.into()), Fourcc::Abgr8888)
            .unwrap();
        let pixels = consumer.map_texture(&mapping).unwrap().to_vec();
        let mut expected = raster(9, 30, retained.pixels);
        tint(&mut expected, shadow.rgba);
        assert_eq!(pixels, expected);
        drop(mapping);
        drop(target);
        // A raw GL name cannot be sampled in an unrelated texture namespace.
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut unrelated = unsafe { GlesRenderer::new(context) }.unwrap();
        let mut texture: GlesTexture = unrelated
            .create_buffer(Fourcc::Abgr8888, (8, 8).into())
            .unwrap();
        let mut target = unrelated.bind(&mut texture).unwrap();
        let mut frame = unrelated
            .render(&mut target, (8, 8).into(), Transform::Normal)
            .unwrap();
        let result: Result<(), _> =
            retained.with_bound(&mut frame, |_| panic!("incompatible context must not draw"));
        assert!(matches!(result, Err(GlesError::MappingError)));
        frame.finish().unwrap().wait().unwrap();
    }

    #[test]
    fn atlas_has_fixed_storage_and_matches_independent_gaussian_convolution() {
        // An independent discrete two-dimensional Gaussian over the rounded
        // source checks the cached convolution's corners, strips and falloff.
        let radius = 9.0;
        let sigma = 15.0;
        let size = 109.0;
        for (x, y) in [(-12.5, -7.5), (3.5, 3.5), (54.5, -4.5), (115.5, 53.5)] {
            let mut expected = 0.0;
            for sy in 0..size as i32 {
                for sx in 0..size as i32 {
                    let px = f64::from(sx) + 0.5;
                    let py = f64::from(sy) + 0.5;
                    let dx = (radius - px).max(px - (size - radius)).max(0.0);
                    let dy = (radius - py).max(py - (size - radius)).max(0.0);
                    let coverage = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
                    expected += coverage
                        * (-((px - x).powi(2) + (py - y).powi(2)) / (2.0 * sigma * sigma)).exp()
                        / (2.0 * std::f64::consts::PI * sigma * sigma);
                }
            }
            assert!((gaussian(x, y, size, size, radius, sigma) - expected).abs() < 0.003);
        }
        let data = raster(256, 512, MAX_SIDE);
        assert_eq!(data.len(), 256 * 256 * 4);
        for (radius, blur) in [(3, 30), (9, 30), (5, 45), (14, 45), (6, 60), (18, 60)] {
            let pad = u32::from(blur) * 3 / 2;
            let rect = 2 * (u32::from(radius) + pad) + 1;
            let side = rect + 2 * pad;
            let pixels = side.min(MAX_SIDE);
            let data = raster(radius, blur, pixels);
            for y in (0..pixels).step_by(5) {
                for x in (0..pixels).step_by(5) {
                    let expected = gaussian(
                        (f64::from(x) + 0.5) * f64::from(side) / f64::from(pixels) - f64::from(pad),
                        (f64::from(y) + 0.5) * f64::from(side) / f64::from(pixels) - f64::from(pad),
                        f64::from(rect),
                        f64::from(rect),
                        f64::from(radius),
                        f64::from(blur) * 0.5,
                    );
                    let actual = f64::from(data[((y * pixels + x) * 4 + 3) as usize]) / 255.0;
                    assert!(
                        (actual - expected).abs() < 0.008,
                        "{radius}/{blur} at {x},{y}: {actual} != {expected}"
                    );
                }
            }
        }
    }
}
