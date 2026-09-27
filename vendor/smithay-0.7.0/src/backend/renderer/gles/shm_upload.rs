//! Optional PBO staging for expensive host texture uploads. The application
//! chooses this policy; the renderer's ordinary import/fence ownership stays
//! intact. GLES retains orphaned buffer storage until queued uploads finish.

use super::ffi;
use crate::utils::{Buffer, Rectangle};

const MIN_UPLOAD: usize = 256 * 1024;

fn source_span(
    len: usize, width: i32, height: i32, stride: i32, pixel_size: usize,
    region: &Rectangle<i32, Buffer>,
) -> Option<(usize, usize)> {
    let (x, y, w, h) = (region.loc.x, region.loc.y, region.size.w, region.size.h);
    if x < 0 || y < 0 || w <= 0 || h <= 0 || stride <= 0 || pixel_size == 0
        || x.checked_add(w)? > width || y.checked_add(h)? > height {
        return None;
    }
    let stride = stride as usize;
    if (width as usize).checked_mul(pixel_size)? > stride { return None; }
    let row = (w as usize).checked_mul(pixel_size)?;
    let active = row.checked_mul(h as usize)?;
    let span = ((h - 1) as usize).checked_mul(stride)?.checked_add(row)?;
    let start = (y as usize).checked_mul(stride)?.checked_add((x as usize).checked_mul(pixel_size)?)?;
    if active < MIN_UPLOAD || span > active.checked_mul(2)?
        || span > isize::MAX as usize || start.checked_add(span)? > len {
        return None;
    }
    Some((start, span))
}

/// The caller holds a current GLES 3 context, a bound texture, and a checked
/// SHM mapping of `len` bytes for this call. Row length matches `stride` and
/// skip offsets are zero. On return the unpack buffer and skips are zero.
/// BufferData copies the source while the SHM access guard is still held.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn stage_subimage(
    gl: &ffi::Gles2, buffer: &mut u32, source: *const u8, len: usize,
    width: i32, height: i32, stride: i32, pixel_size: usize,
    region: &Rectangle<i32, Buffer>, format: u32, type_: u32,
) -> bool {
    let Some((start, span)) = source_span(len, width, height, stride, pixel_size, region) else { return false; };
    if *buffer == 0 { gl.GenBuffers(1, buffer); }
    if *buffer == 0 { return false; }
    gl.BindBuffer(ffi::PIXEL_UNPACK_BUFFER, *buffer);
    // Re-specifying storage preserves earlier in-flight uploads. Never
    // overwrite a mapped buffer still consumed by the GPU.
    gl.BufferData(ffi::PIXEL_UNPACK_BUFFER, span as isize, source.add(start).cast(), ffi::STREAM_DRAW);
    // An allocation failure must not sample old/undersized PBO contents.
    let error = gl.GetError();
    if error != ffi::NO_ERROR {
        tracing::warn!(error, "SHM staging allocation failed; using direct upload");
        gl.BindBuffer(ffi::PIXEL_UNPACK_BUFFER, 0);
        return false;
    }
    let mut allocated = 0i64;
    gl.GetBufferParameteri64v(ffi::PIXEL_UNPACK_BUFFER, ffi::BUFFER_SIZE, &mut allocated);
    if allocated != span as i64 {
        gl.BindBuffer(ffi::PIXEL_UNPACK_BUFFER, 0);
        return false;
    }
    gl.TexSubImage2D(ffi::TEXTURE_2D, 0, region.loc.x, region.loc.y,
        region.size.w, region.size.h, format, type_, std::ptr::null());
    gl.BindBuffer(ffi::PIXEL_UNPACK_BUFFER, 0);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_span_rejects_sparse_small_overflowing_and_out_of_bounds_regions() {
        let rect = Rectangle::new((12, 20).into(), (900, 500).into());
        let stride = 1024 * 4 + 32;
        assert_eq!(source_span(stride * 800, 1024, 800, stride as i32, 4, &rect),
            Some((20 * stride + 48, 499 * stride + 3600)));
        for rect in [
            Rectangle::new((0, 0).into(), (16, 16).into()),
            Rectangle::new((0, 0).into(), (100, 800).into()),
            Rectangle::new((-1, 0).into(), (900, 500).into()),
            Rectangle::new((0, 400).into(), (900, 500).into()),
            Rectangle::new((i32::MAX, 0).into(), (900, 500).into()),
        ] {
            assert!(source_span(stride * 800, 1024, 800, stride as i32, 4, &rect).is_none());
        }
        assert!(source_span(100, 1024, 800, stride as i32, 4, &rect).is_none());
        assert!(source_span(stride * 800, 1024, 800, 10, 4, &rect).is_none());
    }

    #[test]
    #[ignore = "requires GLES 3; validates actual staged uploads and queued PBO storage ownership"]
    fn staged_shm_pixels_match_reference_with_padding_offsets_and_queued_updates() {
        use crate::backend::egl::{native::EGLSurfacelessDisplay, EGLContext, EGLDisplay};
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        unsafe { context.make_current().unwrap(); }
        let gl = ffi::Gles2::load_with(|s| unsafe { crate::backend::egl::get_proc_address(s) as *const _ });
        eprintln!("SHM upload test renderer: {}", unsafe {
            std::ffi::CStr::from_ptr(gl.GetString(ffi::RENDERER).cast()).to_string_lossy()
        });
        let (width, height, stride) = (1024usize, 800usize, 4128usize);
        let mut source = vec![0u8; stride * height + 64];
        let mut expected = [vec![0u8; width * height * 4], vec![0u8; width * height * 4]];
        let mut readback = vec![0u8; width * height * 4];
        let mut textures = [0u32; 2];
        let mut buffer = 0;
        let mut fbo = 0;
        unsafe {
            gl.GenTextures(2, textures.as_mut_ptr());
            gl.GenFramebuffers(1, &mut fbo);
            gl.BindFramebuffer(ffi::FRAMEBUFFER, fbo);
            for (tex, pixels) in textures.iter().zip(&expected) {
                gl.BindTexture(ffi::TEXTURE_2D, *tex);
                gl.TexImage2D(ffi::TEXTURE_2D, 0, ffi::RGBA8 as i32, width as i32, height as i32,
                    0, ffi::RGBA, ffi::UNSIGNED_BYTE, pixels.as_ptr().cast());
            }
            gl.PixelStorei(ffi::UNPACK_ROW_LENGTH, (stride / 4) as i32);
            // Queue updates to two textures without a finish between them.
            // Reusing PBO storage prematurely corrupts the earlier texture.
            for frame in 0..18usize {
                let index = frame % 2;
                let format = if index == 0 { ffi::RGBA } else { ffi::BGRA_EXT };
                let rect = Rectangle::new(((17 + frame) as i32, (29 + frame) as i32).into(), (768, 600).into());
                for y in 0..height {
                    for x in 0..width * 4 {
                        source[64 + y * stride + x] = ((frame * 13 + y * 7 + x * 3) % 251) as u8;
                    }
                }
                gl.BindTexture(ffi::TEXTURE_2D, textures[index]);
                assert!(stage_subimage(&gl, &mut buffer, source.as_ptr().add(64), stride * height,
                    width as i32, height as i32, stride as i32, 4, &rect, format, ffi::UNSIGNED_BYTE));
                for y in rect.loc.y as usize..(rect.loc.y + rect.size.h) as usize {
                    let x = rect.loc.x as usize * 4;
                    let bytes = rect.size.w as usize * 4;
                    expected[index][y * width * 4 + x..y * width * 4 + x + bytes]
                        .copy_from_slice(&source[64 + y * stride + x..64 + y * stride + x + bytes]);
                    if index == 1 {
                        for pixel in expected[index][y * width * 4 + x..y * width * 4 + x + bytes].chunks_exact_mut(4) {
                            pixel.swap(0, 2);
                        }
                    }
                }
                // Invalid/tiny regions take the caller's direct path.
                let small = Rectangle::new((0, 0).into(), (1, 1).into());
                assert!(!stage_subimage(&gl, &mut buffer, source.as_ptr().add(64), stride * height,
                    width as i32, height as i32, stride as i32, 4, &small, format, ffi::UNSIGNED_BYTE));
                gl.TexSubImage2D(ffi::TEXTURE_2D, 0, 0, 0, 1, 1, format, ffi::UNSIGNED_BYTE,
                    source.as_ptr().add(64).cast());
                expected[index][..4].copy_from_slice(&source[64..68]);
                if index == 1 { expected[index].swap(0, 2); }
            }
            for (index, tex) in textures.iter().enumerate() {
                gl.FramebufferTexture2D(ffi::FRAMEBUFFER, ffi::COLOR_ATTACHMENT0, ffi::TEXTURE_2D, *tex, 0);
                assert_eq!(gl.CheckFramebufferStatus(ffi::FRAMEBUFFER), ffi::FRAMEBUFFER_COMPLETE);
                gl.ReadPixels(0, 0, width as i32, height as i32, ffi::RGBA, ffi::UNSIGNED_BYTE, readback.as_mut_ptr().cast());
                assert_eq!(gl.GetError(), ffi::NO_ERROR);
                assert!(readback == expected[index], "all pixels, including undamaged areas, must match texture {index}");
            }
            gl.PixelStorei(ffi::UNPACK_ROW_LENGTH, 0);
            gl.DeleteBuffers(1, &buffer);
            gl.DeleteTextures(2, textures.as_ptr());
            gl.DeleteFramebuffers(1, &fbo);
        }
    }
}
