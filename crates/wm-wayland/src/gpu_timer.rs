//! Optional asynchronous GPU execution measurements. No glFinish, result wait,
//! per-frame allocation, or periodic wakeup is introduced by profiling.

use crate::gpu_stats::Timing;
use smithay::backend::renderer::gles::{ffi, GlesRenderer};
use std::{cell::RefCell, ffi::CStr, fmt::Write, rc::Rc, time::Duration};

const QUERIES: usize = 16;

#[derive(Default)]
pub(crate) struct Measurements {
    pub status: &'static str,
    outputs: Vec<(String, Timing)>,
    pub dropped: u64,
    pub disjoint: u64,
}

impl Measurements {
    pub fn describe(&self, report: &mut String) {
        let _ = writeln!(
            report,
            "gpu_timer status={} pending_capacity={} dropped={} disjoint={}",
            self.status, QUERIES, self.dropped, self.disjoint
        );
        for (name, timing) in &self.outputs {
            let _ = writeln!(report, "gpu_stage output={name:?} stage=composition_gpu samples={} total_ns={} max_ns={} histogram_us_pow2={:?}",
                timing.calls, timing.total_ns, timing.max_ns, timing.histogram);
        }
    }
}

pub(crate) struct GpuTimer {
    enabled: bool,
    initialized: bool,
    supported: bool,
    names: [u32; QUERIES],
    pending: [Option<usize>; QUERIES],
    pub measurements: Rc<RefCell<Measurements>>,
}

impl Default for GpuTimer {
    fn default() -> Self {
        let enabled = std::env::var_os("CHONKSTEP_GPU_TIMINGS").is_some_and(|value| value != "0");
        Self {
            enabled,
            initialized: false,
            supported: false,
            names: [0; QUERIES],
            pending: [None; QUERIES],
            measurements: Rc::new(RefCell::new(Measurements {
                status: if enabled { "pending" } else { "disabled" },
                ..Default::default()
            })),
        }
    }
}

impl GpuTimer {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    fn initialize(&mut self, renderer: &mut GlesRenderer) {
        self.initialized = true;
        self.supported = renderer
            .with_context(|gl| {
                // SAFETY: Smithay made this renderer's context current. GL returns
                // a NUL-terminated extension string owned by that context. Every
                // optional entry point is checked before any extension call.
                unsafe {
                    let extensions = gl.GetString(ffi::EXTENSIONS);
                    if extensions.is_null()
                        || !CStr::from_ptr(extensions.cast())
                            .to_bytes()
                            .split(|byte| *byte == b' ')
                            .any(|ext| ext == b"GL_EXT_disjoint_timer_query")
                        || !gl.GenQueriesEXT.is_loaded()
                        || !gl.DeleteQueriesEXT.is_loaded()
                        || !gl.BeginQueryEXT.is_loaded()
                        || !gl.EndQueryEXT.is_loaded()
                        || !gl.GetQueryivEXT.is_loaded()
                        || !gl.GetQueryObjectuivEXT.is_loaded()
                        || !gl.GetQueryObjectui64vEXT.is_loaded()
                    {
                        return false;
                    }
                    let mut bits = 0;
                    gl.GetQueryivEXT(ffi::TIME_ELAPSED_EXT, ffi::QUERY_COUNTER_BITS_EXT, &mut bits);
                    if bits == 0 {
                        return false;
                    }
                    gl.GenQueriesEXT(QUERIES as i32, self.names.as_mut_ptr());
                    let mut disjoint = 0;
                    gl.GetIntegerv(ffi::GPU_DISJOINT_EXT, &mut disjoint);
                    self.names.iter().all(|name| *name != 0)
                }
            })
            .unwrap_or(false);
        self.measurements.borrow_mut().status = if self.supported {
            "EXT_disjoint_timer_query"
        } else {
            "unavailable"
        };
        tracing::info!(
            supported = self.supported,
            "asynchronous GPU execution timings requested"
        );
    }

    /// The result belongs to a prior frame. Availability checks never wait for
    /// the GPU; a full ring skips measurement while rendering continues.
    pub fn begin(&mut self, renderer: &mut GlesRenderer, output: &str) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        if !self.initialized {
            self.initialize(renderer);
        }
        if !self.supported {
            return None;
        }
        let mut measurements = self.measurements.borrow_mut();
        let index = match measurements.outputs.iter().position(|(name, _)| name == output) {
            Some(index) => index,
            None if measurements.outputs.len() == 64 => {
                measurements.dropped = measurements.dropped.saturating_add(1);
                return None;
            }
            None => {
                measurements.outputs.push((output.to_string(), Timing::default()));
                measurements.outputs.len() - 1
            }
        };
        renderer
            .with_context(|gl| {
                // SAFETY: all names belong to this live context and every pending
                // query has been ended before begin is called again. Reading a
                // result is conditional on QUERY_RESULT_AVAILABLE_EXT.
                unsafe {
                    let mut ready = [None; QUERIES];
                    for (slot, pending) in self.pending.iter().enumerate() {
                        if let Some(output) = pending {
                            let mut available = 0;
                            gl.GetQueryObjectuivEXT(self.names[slot], ffi::QUERY_RESULT_AVAILABLE_EXT, &mut available);
                            if available != 0 {
                                let mut ns = 0;
                                gl.GetQueryObjectui64vEXT(self.names[slot], ffi::QUERY_RESULT_EXT, &mut ns);
                                ready[slot] = Some((*output, ns));
                            }
                        }
                    }
                    // Check after reading: a clock change during the query reads
                    // invalidates those results too. Discard every outstanding
                    // query on a disjoint event, including ones still in flight.
                    let mut disjoint = 0;
                    gl.GetIntegerv(ffi::GPU_DISJOINT_EXT, &mut disjoint);
                    if disjoint != 0 {
                        measurements.disjoint = measurements.disjoint.saturating_add(1);
                        measurements.dropped = measurements
                            .dropped
                            .saturating_add(self.pending.iter().flatten().count() as u64);
                        self.pending.fill(None);
                    } else {
                        for (slot, result) in ready.into_iter().enumerate() {
                            if let Some((output, ns)) = result {
                                measurements.outputs[output].1.record(Duration::from_nanos(ns));
                                self.pending[slot] = None;
                            }
                        }
                    }
                    let Some(slot) = self.pending.iter().position(Option::is_none) else {
                        measurements.dropped = measurements.dropped.saturating_add(1);
                        return None;
                    };
                    gl.BeginQueryEXT(ffi::TIME_ELAPSED_EXT, self.names[slot]);
                    self.pending[slot] = Some(index);
                    Some(slot)
                }
            })
            .ok()
            .flatten()
    }

    pub fn end(&mut self, renderer: &mut GlesRenderer, token: Option<usize>) {
        if token.is_none() {
            return;
        }
        use smithay::backend::egl::ffi::egl;
        // SAFETY: EGL's current bindings are thread-local. Keep the exact
        // display/context/surfaces alive through this synchronous call; the
        // winit target must still be current when its buffers are swapped.
        let (display, context, draw, read) = unsafe {
            (
                egl::GetCurrentDisplay(),
                egl::GetCurrentContext(),
                egl::GetCurrentSurface(egl::DRAW as i32),
                egl::GetCurrentSurface(egl::READ as i32),
            )
        };
        let result = renderer.with_context(|gl| {
            // SAFETY: this is the paired end for the successful begin on this
            // renderer's context; no timer queries are nested in our pipeline.
            unsafe {
                gl.EndQueryEXT(ffi::TIME_ELAPSED_EXT);
            }
        });
        // SAFETY: these are the same live EGL handles saved above, on the
        // same thread, without any target destruction in between.
        let restored =
            unsafe { display == egl::NO_DISPLAY || egl::MakeCurrent(display, draw, read, context) == egl::TRUE };
        if result.is_err() || !restored {
            self.supported = false;
            self.measurements.borrow_mut().status = "context-error";
        }
    }
}

// Query names have exactly the renderer-context lifetime. They are allocated
// once (16 total), reused in place, and reclaimed by GL on context destruction.
