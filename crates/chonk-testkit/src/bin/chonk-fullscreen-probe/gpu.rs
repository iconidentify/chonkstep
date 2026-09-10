//! Optional real Wayland EGL producer for the scaling benchmark. The ordinary
//! conformance fixture keeps its SHM path; request EGL explicitly to measure
//! DMA-BUF composition without a full-frame CPU texture upload each commit.

#[cfg(feature = "gpu-probe")]
mod egl {
    use std::{ffi::{c_char, c_int, c_void, CStr}, ptr};
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
        fn glCreateShader(kind: u32) -> u32;
        fn glShaderSource(shader: u32, count: c_int, source: *const *const c_char, length: *const c_int);
        fn glCompileShader(shader: u32);
        fn glGetShaderiv(shader: u32, name: u32, value: *mut c_int);
        fn glGetShaderInfoLog(shader: u32, size: c_int, length: *mut c_int, log: *mut c_char);
        fn glDeleteShader(shader: u32);
        fn glCreateProgram() -> u32;
        fn glAttachShader(program: u32, shader: u32);
        fn glLinkProgram(program: u32);
        fn glGetProgramiv(program: u32, name: u32, value: *mut c_int);
        fn glGetProgramInfoLog(program: u32, size: c_int, length: *mut c_int, log: *mut c_char);
        fn glDeleteProgram(program: u32);
        fn glUseProgram(program: u32);
        fn glGetAttribLocation(program: u32, name: *const c_char) -> c_int;
        fn glGetUniformLocation(program: u32, name: *const c_char) -> c_int;
        fn glUniform2f(location: c_int, x: f32, y: f32);
        fn glGenBuffers(count: c_int, buffers: *mut u32);
        fn glBindBuffer(target: u32, buffer: u32);
        fn glBufferData(target: u32, size: isize, data: *const c_void, usage: u32);
        fn glDeleteBuffers(count: c_int, buffers: *const u32);
        fn glEnableVertexAttribArray(index: u32);
        fn glVertexAttribPointer(index: u32, size: c_int, kind: u32, normalized: u8, stride: c_int, pointer: *const c_void);
        fn glDrawArrays(mode: u32, first: c_int, count: c_int);
    }

    pub struct Gpu {
        display: Handle,
        context: Handle,
        target: Handle,
        window: WlEglSurface,
        pattern: Option<Pattern>,
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
                let pattern = match std::env::var("CHONKSTEP_PROBE_GPU_PATTERN").as_deref() {
                    Ok("texture") => Some(Pattern::new()),
                    Ok("solid") | Err(_) => None,
                    Ok(other) => panic!("unknown GPU fixture pattern {other}"),
                };
                super::super::say(&format!("GPU client pattern={}", if pattern.is_some() { "texture" } else { "solid" }));
                Self { display, context, target, window, pattern, _surface: surface.clone(), _connection: connection.clone() }
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
                if let Some(pattern) = &self.pattern {
                    pattern.draw(width, height);
                } else {
                    glClearColor(32.0 / 255.0, 64.0 / 255.0, 128.0 / 255.0, 1.0);
                    glClear(0x4000);
                }
                glFinish();
                assert_eq!(eglSwapBuffers(self.display, self.target), 1, "EGL buffer submission");
            }
        }
    }

    struct Pattern { program: u32, buffer: u32, position: u32, size: c_int }

    impl Pattern {
        fn new() -> Self {
            let vertex = c"attribute vec2 position; void main() { gl_Position=vec4(position,0.0,1.0); }";
            let fragment = c"precision highp float; uniform vec2 size;
                void main() {
                    vec2 uv=gl_FragCoord.xy/size;
                    float n=fract(52.9829189*fract(dot(floor(gl_FragCoord.xy),vec2(0.06711056,0.00583715))));
                    vec3 color=vec3(n,fract(n*17.0),fract(n*31.0))*0.75+0.1;
                    if(uv.x<0.025 || uv.x>0.975 || (abs(uv.x-0.5)<0.015 && abs(uv.y-0.5)<0.015))
                        color=vec3(32.0,64.0,128.0)/255.0;
                    gl_FragColor=vec4(color,1.0);
                }";
            // SAFETY: Gpu::new has made its live GLES context current. Shader
            // strings are NUL terminated; log buffers and vertex storage have
            // exact lengths passed to GL. Every compile/link result is checked.
            unsafe {
                let program = glCreateProgram();
                assert_ne!(program, 0);
                for (kind, source) in [(0x8B31, vertex), (0x8B30, fragment)] {
                    let shader = glCreateShader(kind);
                    assert_ne!(shader, 0);
                    glShaderSource(shader, 1, &source.as_ptr(), ptr::null());
                    glCompileShader(shader);
                    let mut ok = 0;
                    glGetShaderiv(shader, 0x8B81, &mut ok);
                    let mut log = [0u8; 2048];
                    glGetShaderInfoLog(shader, log.len() as c_int, ptr::null_mut(), log.as_mut_ptr().cast());
                    assert_eq!(ok, 1, "shader compilation: {}", String::from_utf8_lossy(&log));
                    glAttachShader(program, shader);
                    glDeleteShader(shader);
                }
                glLinkProgram(program);
                let mut ok = 0;
                glGetProgramiv(program, 0x8B82, &mut ok);
                let mut log = [0u8; 2048];
                glGetProgramInfoLog(program, log.len() as c_int, ptr::null_mut(), log.as_mut_ptr().cast());
                assert_eq!(ok, 1, "shader linking: {}", String::from_utf8_lossy(&log));
                let position = glGetAttribLocation(program, c"position".as_ptr());
                let size = glGetUniformLocation(program, c"size".as_ptr());
                assert!(position >= 0 && size >= 0);
                let vertices = [-1.0f32, -1.0, 3.0, -1.0, -1.0, 3.0];
                let mut buffer = 0;
                glGenBuffers(1, &mut buffer);
                assert_ne!(buffer, 0);
                glBindBuffer(0x8892, buffer);
                glBufferData(0x8892, std::mem::size_of_val(&vertices) as isize, vertices.as_ptr().cast(), 0x88E4);
                Self { program, buffer, position: position as u32, size }
            }
        }
        fn draw(&self, width: i32, height: i32) {
            // SAFETY: the owning Gpu keeps this context current and owns the
            // program/VBO until after the last draw. The bound VBO holds three
            // vec2 vertices; the zero offset and stride cannot read past it.
            unsafe {
                glUseProgram(self.program);
                glUniform2f(self.size, width as f32, height as f32);
                glBindBuffer(0x8892, self.buffer);
                glEnableVertexAttribArray(self.position);
                glVertexAttribPointer(self.position, 2, 0x1406, 0, 0, ptr::null());
                glDrawArrays(4, 0, 3);
            }
        }
    }

    impl Drop for Gpu {
        fn drop(&mut self) {
            // SAFETY: no EGL call is in flight; the Wayland window, surface and
            // display remain alive until all EGL resources have been destroyed.
            unsafe {
                eglMakeCurrent(self.display, self.target, self.target, self.context);
                if let Some(pattern) = self.pattern.take() {
                    glUseProgram(0);
                    glDeleteProgram(pattern.program);
                    glDeleteBuffers(1, &pattern.buffer);
                }
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
