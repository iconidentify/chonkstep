//! Optional real Wayland EGL producer for the scaling benchmark. The ordinary
//! conformance fixture keeps its SHM path; request EGL explicitly to measure
//! DMA-BUF composition without a full-frame CPU texture upload each commit.

#[cfg(feature = "gpu-probe")]
mod egl {
    use std::{ffi::{c_int, c_void, CStr}, ptr};
    use wayland_client::{protocol::wl_surface::WlSurface, Connection, Proxy};
    use wayland_egl::WlEglSurface;

    type Handle = *mut c_void;
    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetPlatformDisplay(platform: u32, display: Handle, attributes: *const isize) -> Handle;
        fn eglInitialize(display: Handle, major: *mut c_int, minor: *mut c_int) -> u32;
        fn eglBindAPI(api: u32) -> u32;
        fn eglChooseConfig(display: Handle, attributes: *const c_int, configs: *mut Handle, size: c_int, count: *mut c_int) -> u32;
        fn eglCreateContext(display: Handle, config: Handle, share: Handle, attributes: *const c_int) -> Handle;
        fn eglCreateWindowSurface(display: Handle, config: Handle, window: Handle, attributes: *const c_int) -> Handle;
        fn eglMakeCurrent(display: Handle, draw: Handle, read: Handle, context: Handle) -> u32;
        fn eglSwapInterval(display: Handle, interval: c_int) -> u32;
        fn eglSwapBuffers(display: Handle, surface: Handle) -> u32;
        fn eglDestroySurface(display: Handle, surface: Handle) -> u32;
        fn eglDestroyContext(display: Handle, context: Handle) -> u32;
        fn eglTerminate(display: Handle) -> u32;
    }
    #[link(name = "GLESv2")]
    unsafe extern "C" {
        fn glViewport(x: c_int, y: c_int, width: c_int, height: c_int);
        fn glClearColor(red: f32, green: f32, blue: f32, alpha: f32);
        fn glClear(mask: u32);
        fn glFinish();
        fn glGetString(name: u32) -> *const u8;
    }

    pub struct Gpu {
        display: Handle,
        context: Handle,
        target: Handle,
        window: WlEglSurface,
        // Keep the native objects alive until after Drop destroys EGL.
        _surface: WlSurface,
        _connection: Connection,
    }

    impl Gpu {
        pub fn new(connection: &Connection, surface: &WlSurface) -> Self {
            let window = WlEglSurface::new(surface.id(), 1, 1).expect("Wayland EGL window");
            // SAFETY: the connection uses libwayland (gpu-probe feature), and
            // the stored clones keep its display and wl_surface alive. Every
            // attribute array is EGL_NONE terminated, and every returned handle
            // is checked before use. No other GL context exists in this client.
            unsafe {
                let display = eglGetPlatformDisplay(0x31D8, connection.display().id().as_ptr().cast(), ptr::null());
                assert!(!display.is_null(), "EGL Wayland display");
                assert_eq!(eglInitialize(display, ptr::null_mut(), ptr::null_mut()), 1, "EGL initialize");
                assert_eq!(eglBindAPI(0x30A0), 1, "EGL OpenGL ES API");
                let attributes = [0x3033, 4, 0x3040, 4, 0x3024, 8, 0x3023, 8, 0x3022, 8, 0x3021, 0, 0x3038];
                let mut config = ptr::null_mut();
                let mut count = 0;
                assert_eq!(eglChooseConfig(display, attributes.as_ptr(), &mut config, 1, &mut count), 1);
                assert!(count > 0, "an RGB8 EGL window configuration");
                let context = eglCreateContext(display, config, ptr::null_mut(), [0x3098, 2, 0x3038].as_ptr());
                assert!(!context.is_null(), "EGL GLES2 context");
                let target = eglCreateWindowSurface(display, config, window.ptr().cast_mut(), [0x3038].as_ptr());
                assert!(!target.is_null(), "EGL window surface");
                assert_eq!(eglMakeCurrent(display, target, target, context), 1);
                // wl_surface.frame in the parent fixture controls cadence.
                assert_eq!(eglSwapInterval(display, 0), 1);
                let name = glGetString(0x1F01);
                assert!(!name.is_null());
                super::super::say(&format!("GPU client renderer={}", CStr::from_ptr(name.cast()).to_string_lossy()));
                Self { display, context, target, window, _surface: surface.clone(), _connection: connection.clone() }
            }
        }

        pub fn draw(&self, width: i32, height: i32) {
            self.window.resize(width, height, 0, 0);
            // SAFETY: this fixture is single-threaded, this context remains
            // current, and these are live handles created in new. Finish is
            // deliberately on the PRODUCER: NVIDIA clients without explicit
            // sync must complete writes before handing a DMA-BUF to the server.
            // The compositor's independently timed rendering never calls it.
            unsafe {
                glViewport(0, 0, width, height);
                glClearColor(32.0 / 255.0, 64.0 / 255.0, 128.0 / 255.0, 1.0);
                glClear(0x4000);
                glFinish();
                assert_eq!(eglSwapBuffers(self.display, self.target), 1, "EGL buffer submission");
            }
        }
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            // SAFETY: no EGL call is in flight; the Wayland window, surface and
            // display remain alive until all EGL resources have been destroyed.
            unsafe {
                eglMakeCurrent(self.display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
                eglDestroySurface(self.display, self.target);
                eglDestroyContext(self.display, self.context);
                eglTerminate(self.display);
            }
        }
    }
}

#[cfg(feature = "gpu-probe")]
pub use egl::Gpu;

#[cfg(not(feature = "gpu-probe"))]
pub enum Gpu {}
#[cfg(not(feature = "gpu-probe"))]
impl Gpu {
    pub fn new(_: &wayland_client::Connection, _: &wayland_client::protocol::wl_surface::WlSurface) -> Self {
        super::fatal("EGL producer requires cargo build -p chonk-testkit --features gpu-probe")
    }
    pub fn draw(&self, _: i32, _: i32) { match *self {} }
}
