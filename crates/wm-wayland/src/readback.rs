//! RGBA8 capture on both GLES 3 and the renderer's GLES 2 baseline.
//!
//! Smithay 0.7's ExportMem path unconditionally uses GLES 3 pixel-pack
//! buffers, STREAM_READ and MapBufferRange. The GLES 2 renderer can draw
//! correctly while that path fails. Keep its mapped-buffer path on newer
//! contexts; only older contexts download directly into owned CPU memory.

use std::{cell::Cell, sync::atomic::{AtomicU64, Ordering}, time::{Duration, Instant}};
use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::fence::EGLFence;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::backend::renderer::gles::{ffi, Capability, GlesMapping, GlesRenderer, GlesTarget};
use smithay::backend::renderer::{ExportMem, Frame, Renderer};
use smithay::utils::{Buffer, Physical, Rectangle, Size, Transform};

/// One owned download, retaining the GLES mapping without cloning its pixels.
/// This lets a capture fanout yield between client copies. The GLES 2 fallback
/// owns the same CPU buffer its immediate readback already required.
enum Storage {
    Mapped(GlesMapping),
    Owned(Vec<u8>),
}

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(5);
static QUEUED: AtomicU64 = AtomicU64::new(0);
static DEFERRED: AtomicU64 = AtomicU64::new(0);
static SYNCHRONOUS: AtomicU64 = AtomicU64::new(0);
static ACTIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static PEAK_BYTES: AtomicU64 = AtomicU64::new(0);
static MAPS: AtomicU64 = AtomicU64::new(0);
static MAP_NS: AtomicU64 = AtomicU64::new(0);

pub(crate) struct RgbaDownload {
    storage: Storage,
    completion: SyncPoint,
    ready: Cell<bool>,
    started: Instant,
    ready_after: Instant,
    bytes: u64,
    // The producer must not receive wl_buffer.release while the GPU can
    // still sample it. Cancellation retires this whole download until ready.
    scene: Vec<crate::renderer::SceneElement<GlesRenderer>>,
}

impl RgbaDownload {
    fn new(storage: Storage, completion: SyncPoint, bytes: usize) -> Self {
        QUEUED.fetch_add(1, Ordering::Relaxed);
        let active = ACTIVE_BYTES.fetch_add(bytes as u64, Ordering::Relaxed) + bytes as u64;
        PEAK_BYTES.fetch_max(active, Ordering::Relaxed);
        let started = Instant::now();
        // Fault injection is confined to an explicitly enabled private test
        // door. No sleep or busy wait: ordinary service deadlines still drive it.
        static TEST_DELAY: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
        let delay = *TEST_DELAY.get_or_init(|| {
            if std::env::var_os("CHONKSTEP_TEST_SOCKET").is_none() { return Duration::ZERO; }
            Duration::from_millis(std::env::var("CHONKSTEP_TEST_READBACK_DELAY_MS").ok()
                .and_then(|value| value.parse::<u64>().ok()).unwrap_or(0).min(10_000))
        });
        Self { storage, completion, ready_after: started + delay, ready: Cell::new(false), started, bytes: bytes as u64, scene: Vec::new() }
    }

    pub fn hold_scene(&mut self, scene: &mut Vec<crate::renderer::SceneElement<GlesRenderer>>) {
        self.scene.append(scene);
    }

    pub fn ready(&self) -> bool {
        if self.ready.get() { return true; }
        let ready = Instant::now() >= self.ready_after && self.completion.is_reached();
        self.ready.set(ready);
        if !ready { DEFERRED.fetch_add(1, Ordering::Relaxed); }
        ready
    }

    pub fn release_scene(&mut self) {
        if self.ready() { self.scene.clear(); }
    }

    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= DOWNLOAD_TIMEOUT
    }

    /// Calling code must yield until the fence is ready. Enforcing this at
    /// the mapping boundary prevents a later caller from reintroducing a stall.
    pub(crate) fn with_pixels<R>(
        &self,
        renderer: &mut GlesRenderer,
        consume: impl FnOnce(&[u8]) -> R,
    ) -> Result<R, String> {
        if !self.ready() { return Err("readback is still pending".into()); }
        match &self.storage {
            Storage::Mapped(mapping) => {
                let started = Instant::now();
                let pixels = renderer.map_texture(mapping).map_err(|error| format!("map: {error:?}"))?;
                MAPS.fetch_add(1, Ordering::Relaxed);
                MAP_NS.fetch_add(started.elapsed().as_nanos().min(u64::MAX as u128) as u64, Ordering::Relaxed);
                Ok(consume(pixels))
            }
            Storage::Owned(pixels) => Ok(consume(pixels)),
        }
    }
}

impl Drop for RgbaDownload {
    fn drop(&mut self) { ACTIVE_BYTES.fetch_sub(self.bytes, Ordering::Relaxed); }
}

pub(crate) fn diagnostics() -> String {
    format!("readback queued={} pending_polls={} synchronous_fallbacks={} active_bytes={} peak_bytes={} maps={} map_cpu_ns={}\n",
        QUEUED.load(Ordering::Relaxed), DEFERRED.load(Ordering::Relaxed), SYNCHRONOUS.load(Ordering::Relaxed),
        ACTIVE_BYTES.load(Ordering::Relaxed), PEAK_BYTES.load(Ordering::Relaxed), MAPS.load(Ordering::Relaxed), MAP_NS.load(Ordering::Relaxed))
}

pub(crate) fn download_rgba(
    renderer: &mut GlesRenderer,
    framebuffer: &mut GlesTarget<'_>,
    size: Size<i32, Buffer>,
) -> Result<RgbaDownload, String> {
    let length = rgba_length(size.w, size.h).ok_or("invalid RGBA readback extent")?;
    // In Smithay 0.7, Blit is a core-GLES-3-only capability. Unlike
    // re-querying GL_VERSION on every capture this is already cached.
    if renderer.capabilities().contains(&Capability::Blit) {
        let mapping = renderer
            .copy_framebuffer(framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
            .map_err(|error| format!("readback: {error:?}"))?;
        // The fence MUST follow the PBO read command, not just the scene's
        // drawing fence: that earlier fence does not cover GPU-to-PBO transfer.
        let fence = if renderer.capabilities().contains(&Capability::ExportFence) {
            EGLFence::create(renderer.egl_context().display()).ok()
        } else { None };
        let completion = if let Some(fence) = fence {
            renderer.with_context(|gl| {
                // SAFETY: Smithay made this exact GLES context current. Flush
                // submits queued work/fences without waiting for completion.
                unsafe { gl.Flush(); }
            }).map_err(|error| format!("readback flush: {error:?}"))?;
            SyncPoint::from(fence)
        } else {
            // Older contexts retain the synchronous behavior explicitly. Once
            // mapped, the stored PBO is complete and safe for deferred copying.
            SYNCHRONOUS.fetch_add(1, Ordering::Relaxed);
            renderer.map_texture(&mapping).map_err(|error| format!("readback fallback map: {error:?}"))?;
            SyncPoint::signaled()
        };
        return Ok(RgbaDownload::new(Storage::Mapped(mapping), completion, length));
    }

    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(length)
        .map_err(|error| format!("readback allocation: {error}"))?;
    pixels.resize(length, 0);
    // A no-draw frame binds this exact target through Smithay's public
    // API. Merely calling renderer.with_context would make the context
    // current without guaranteeing which framebuffer it had retained.
    let mut frame = renderer
        .render(
            framebuffer,
            Size::<i32, Physical>::from((size.w, size.h)),
            Transform::Normal,
        )
        .map_err(|error| format!("readback target: {error:?}"))?;
    let error = frame
        .with_context(|gl| {
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
                gl.ReadPixels(
                    0,
                    0,
                    size.w,
                    size.h,
                    ffi::RGBA,
                    ffi::UNSIGNED_BYTE,
                    pixels.as_mut_ptr().cast(),
                );
                let error = gl.GetError();
                gl.PixelStorei(ffi::PACK_ALIGNMENT, alignment);
                error
            }
        })
        .map_err(|error| format!("readback context: {error:?}"))?;
    // ReadPixels has already completed the download; no later CPU use
    // depends on the fence that finishing this no-draw frame may create.
    let _sync = frame
        .finish()
        .map_err(|error| format!("readback finish: {error:?}"))?;
    if error != ffi::NO_ERROR {
        return Err(format!("GLES 2 ReadPixels error: {error:#x}"));
    }
    SYNCHRONOUS.fetch_add(1, Ordering::Relaxed);
    Ok(RgbaDownload::new(Storage::Owned(pixels), SyncPoint::signaled(), length))
}

// Validate raw dimensions: Smithay's Size constructor debug-asserts on
// negative inputs before our allocation guard could inspect them. Keeping
// this arithmetic independent of Size tests the same rejection in every profile.
fn rgba_length(width: i32, height: i32) -> Option<usize> {
    if width <= 0 || height <= 0 {
        return None;
    }
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
        // Smithay 0.7's PBO allocation and mapping multiply these dimensions
        // in i32, even on 64-bit hosts. Reject before those signed products.
        .filter(|length| *length <= i32::MAX as usize && *length <= isize::MAX as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_readback_extents_count_four_bytes_per_pixel() {
        assert_eq!(rgba_length(1, 1), Some(4));
        assert_eq!(rgba_length(1280, 800), Some(4_096_000));
    }

    #[test]
    fn empty_and_negative_readback_extents_are_rejected_without_constructing_geometry() {
        for (width, height) in [
            (0, 0),
            (0, 1),
            (1, 0),
            (-1, 1),
            (1, -1),
            (-1, -1),
            (i32::MIN, 1),
            (1, i32::MIN),
        ] {
            assert_eq!(rgba_length(width, height), None, "extent {width}x{height}");
        }
    }

    #[test]
    fn oversized_readback_extents_are_rejected_before_allocating_or_calling_gl() {
        // Exceeds isize::MAX on 64-bit targets and overflows usize on 32-bit targets.
        assert_eq!(rgba_length(i32::MAX, i32::MAX), None);
        assert_eq!(rgba_length(32768, 16384), None);
        assert_eq!(rgba_length(32767, 16384), Some(2_147_418_112));
    }
}
