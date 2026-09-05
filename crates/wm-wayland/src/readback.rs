//! RGBA8 capture on both GLES 3 and the renderer's GLES 2 baseline.
//!
//! Smithay 0.7's ExportMem path unconditionally uses GLES 3 pixel-pack
//! buffers, STREAM_READ and MapBufferRange. The GLES 2 renderer can draw
//! correctly while that path fails. Keep its mapped-buffer path on newer
//! contexts; only older contexts download directly into owned CPU memory.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{ffi, Capability, GlesRenderer, GlesTarget};
use smithay::backend::renderer::{ExportMem, Frame, Renderer};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

/// Borrow the downloaded pixels for `consume`, without adding an owned
/// copy to the GLES 3 path. Targets here are RGBA8 offscreen textures,
/// with the same top-row-first convention as Smithay's ExportMem output.
pub(crate) fn with_rgba_pixels<R>(
    renderer: &mut GlesRenderer,
    framebuffer: &mut GlesTarget<'_>,
    size: Size<i32, Buffer>,
    consume: impl FnOnce(&[u8]) -> R,
) -> Result<R, String> {
    let length = rgba_length(size).ok_or("invalid RGBA readback extent")?;
    // In Smithay 0.7, Blit is a core-GLES-3-only capability. Unlike
    // re-querying GL_VERSION on every capture this is already cached.
    if renderer.capabilities().contains(&Capability::Blit) {
        let mapping = renderer.copy_framebuffer(framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
            .map_err(|error| format!("readback: {error:?}"))?;
        let pixels = renderer.map_texture(&mapping).map_err(|error| format!("map: {error:?}"))?;
        return Ok(consume(pixels));
    }

    let mut pixels = Vec::new();
    pixels.try_reserve_exact(length).map_err(|error| format!("readback allocation: {error}"))?;
    pixels.resize(length, 0);
    // A no-draw frame binds this exact target through Smithay's public
    // API. Merely calling renderer.with_context would make the context
    // current without guaranteeing which framebuffer it had retained.
    let mut frame = renderer.render(framebuffer, Size::<i32, Physical>::from((size.w, size.h)), Transform::Normal)
        .map_err(|error| format!("readback target: {error:?}"))?;
    let error = frame.with_context(|gl| {
        // SAFETY: the frame owns the current context and target. The
        // initialized destination contains exactly width*height*4 bytes;
        // RGBA/UNSIGNED_BYTE and PACK_ALIGNMENT=1 cannot exceed it. No
        // pixel-pack buffer is used by this GLES 2 path. Restore the one
        // GL setting changed here before returning control to Smithay.
        unsafe {
            let mut alignment = 0;
            gl.GetIntegerv(ffi::PACK_ALIGNMENT, &mut alignment);
            gl.PixelStorei(ffi::PACK_ALIGNMENT, 1);
            gl.GetError();
            gl.ReadPixels(0, 0, size.w, size.h, ffi::RGBA, ffi::UNSIGNED_BYTE, pixels.as_mut_ptr().cast());
            let error = gl.GetError();
            gl.PixelStorei(ffi::PACK_ALIGNMENT, alignment);
            error
        }
    }).map_err(|error| format!("readback context: {error:?}"))?;
    // ReadPixels has already completed the download; no later CPU use
    // depends on the fence that finishing this no-draw frame may create.
    let _sync = frame.finish().map_err(|error| format!("readback finish: {error:?}"))?;
    if error != ffi::NO_ERROR {
        return Err(format!("GLES 2 ReadPixels error: {error:#x}"));
    }
    Ok(consume(&pixels))
}

fn rgba_length(size: Size<i32, Buffer>) -> Option<usize> {
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    (size.w as usize).checked_mul(size.h as usize)?.checked_mul(4)
        .filter(|length| *length <= isize::MAX as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_extents_are_checked_before_allocating_or_calling_gl() {
        assert_eq!(rgba_length((1280, 800).into()), Some(4_096_000));
        for size in [(0, 1), (1, 0), (-1, 1), (1, -1), (i32::MAX, i32::MAX)] {
            assert_eq!(rgba_length(size.into()), None);
        }
    }
}
