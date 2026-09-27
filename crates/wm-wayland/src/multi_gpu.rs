//! Explicit render-GPU / target-GPU composition. Single-device sessions keep
//! their original GLES renderer; cross-device copies are opt-in at startup.
//!
//! The target is normally another GPU's render node. A KMS device with no
//! render node at all (simpledrm on a boot framebuffer, as on Apple silicon
//! without a display driver) is a *display-only* target: its primary node
//! names it inside the GPU manager, and the target renderer must be the
//! software renderer bound to that KMS fd's dumb buffers.
use std::ffi::CStr;
use std::path::Path;

use smithay::backend::allocator::dmabuf::AsDmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Buffer as AllocatedBuffer, Format, Fourcc};
use smithay::backend::drm::{DrmNode, NodeType};
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::element::{RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::{ffi, GlesRenderer};
use smithay::backend::renderer::multigpu::{gbm::GbmGlesBackend, ApiDevice, GpuManager, MultiRenderer};
use smithay::backend::renderer::{ImportDma, Renderer as RendererTrait, RendererSuper};
use smithay::utils::{Buffer, DeviceFd, Physical, Rectangle};

use crate::renderer::SceneElement;

pub(crate) type Api = GbmGlesBackend<GlesRenderer, DeviceFd>;
pub(crate) type Renderer<'a> = MultiRenderer<'a, 'a, Api, Api>;

pub(crate) enum Stack {
    Single(Box<GlesRenderer>),
    Multi(Box<CrossGpu>),
}

pub(crate) struct CrossGpu {
    manager: GpuManager<Api>,
    pub render: DrmNode,
    pub target: DrmNode,
    pub scanout_imports: FormatSet,
    /// `target` is the primary node of a KMS device that has no render
    /// node. Its renderer is proven to be software, no cross-device
    /// scanout tranche is probed, and client buffers are never offered to
    /// its planes (see [`Stack::client_scanout_node`]).
    pub display_only: bool,
}

/// Which DRM identity stands for the KMS device inside the GPU manager.
///
/// A proven render node wins; that is the original rule, and on split
/// display/render hardware it names the GPU that really draws into the
/// swapchain. Only when there is none, and the KMS device itself has no
/// render node, does its primary node name the target: the GBM device and
/// EGL display the manager creates for it are opened on exactly that KMS
/// fd. This is an identity inside the manager only. It never becomes the
/// session's render node and never reaches linux-dmabuf feedback, where a
/// display-only `dev_t` would promise clients a device they cannot render
/// on.
pub(crate) fn target_node(render_node: Option<DrmNode>, kms: Option<DrmNode>) -> Option<DrmNode> {
    select_target(render_node, kms.map(|node| (node, node.ty(), node.has_render())))
}

/// The pure half of [`target_node`], generic so tests need no `/dev/dri`.
fn select_target<N>(render_node: Option<N>, kms: Option<(N, NodeType, bool)>) -> Option<N> {
    render_node.or_else(|| kms.and_then(|(node, ty, has_render)| is_display_only(ty, has_render).then_some(node)))
}

/// A primary node without a render node: KMS with nothing to render on.
/// A primary that *has* a render node but no proven pairing stays an error
/// rather than being guessed at, as before.
fn is_display_only(ty: NodeType, has_render: bool) -> bool {
    ty == NodeType::Primary && !has_render
}

/// What an EGL display says about the device it renders on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EglIdentity<N> {
    /// `EGL_EXT_device_drm(_render_node)` named this render node.
    RenderNode(N),
    /// `EGL_MESA_device_software`: a CPU renderer (llvmpipe, kms_swrast).
    Software,
    /// Neither; the renderer's device cannot be established.
    Unidentified,
}

fn egl_identity(renderer: &GlesRenderer) -> EglIdentity<DrmNode> {
    if let Some(node) = crate::dmabuf::render_node_for_renderer(renderer) {
        return EglIdentity::RenderNode(node);
    }
    match EGLDevice::device_for_display(renderer.egl_context().display()) {
        Ok(device) if device.is_software() => EglIdentity::Software,
        _ => EglIdentity::Unidentified,
    }
}

/// The identity of a display-only target's renderer. What draws is asked
/// first: Mesa's EGL device query cannot tell the two cases apart here. A
/// kms_swrast screen chosen by driver name (a drirc `dri_driver`) is not
/// flagged software, so EGL names the display's "compatible render-only
/// device" exactly as it does for kmsro, which really renders on that GPU.
fn display_target_identity(renderer: &mut GlesRenderer) -> (EglIdentity<DrmNode>, Option<String>) {
    let name = gl_renderer_name(renderer);
    if name.as_deref().is_some_and(is_software_gl_renderer) {
        return (EglIdentity::Software, name);
    }
    (egl_identity(renderer), name)
}

/// Whether `renderer` is one of Mesa's CPU rasterizers, by the name it reports
/// in `GL_RENDERER`. Unlike the EGL device query, this cannot be misled by a
/// kms_swrast screen that names the display's render-only companion device.
pub(crate) fn renderer_is_software(renderer: &mut GlesRenderer) -> bool {
    gl_renderer_name(renderer).as_deref().is_some_and(is_software_gl_renderer)
}

fn gl_renderer_name(renderer: &mut GlesRenderer) -> Option<String> {
    renderer
        .with_context(|gl| {
            // SAFETY: Smithay made this renderer's context current for the
            // callback. GL returns null or a NUL-terminated RENDERER string
            // owned by that context, copied before the callback returns.
            unsafe {
                let name = gl.GetString(ffi::RENDERER);
                (!name.is_null()).then(|| CStr::from_ptr(name.cast()).to_string_lossy().into_owned())
            }
        })
        .ok()
        .flatten()
}

/// Mesa's CPU rasterizers, as they name themselves in `GL_RENDERER`.
/// Zink is never one, whatever Vulkan device it runs on.
fn is_software_gl_renderer(name: &str) -> bool {
    name.starts_with("llvmpipe") || name.starts_with("softpipe")
}

/// Whether a manager renderer is the device it was created for.
///
/// A render-node device must be named by EGL exactly (the original rule).
/// A display-only target must be a software renderer: that is what draws
/// into the KMS fd's own dumb buffers. A GPU found *behind* a display-only
/// fd (Mesa's kmsro wrapping some render node) is refused, because the
/// session would then copy into a second GPU it neither selected nor
/// identified.
fn proves_device<N: PartialEq>(identity: EglIdentity<N>, node: N, display_only: bool) -> bool {
    match identity {
        EglIdentity::RenderNode(named) => !display_only && named == node,
        EglIdentity::Software => display_only,
        EglIdentity::Unidentified => false,
    }
}

impl CrossGpu {
    /// Both nodes are fixed for this session. Render-node handles need no DRM
    /// master or modeset permission; the session's KMS handle stays separate.
    pub fn new(render_path: &Path, target: DrmNode, target_fd: DeviceFd) -> Result<Self, String> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(render_path)
            .map_err(|error| format!("open render device {}: {error}", render_path.display()))?;
        let render = DrmNode::from_file(&file).map_err(|error| format!("render device identity: {error}"))?;
        if render.ty() != NodeType::Render {
            return Err("CHONKSTEP_RENDER_DEVICE must name a DRM render node".into());
        }
        if render == target {
            return Err("render and target devices are the same; use the single-GPU path".into());
        }
        let display_only = target.ty() == NodeType::Primary;
        let render_fd = DeviceFd::from(std::os::fd::OwnedFd::from(file));
        let mut api = Api::default();
        for (node, fd) in [(render, render_fd), (target, target_fd.clone())] {
            let gbm = GbmDevice::new(fd).map_err(|error| format!("GBM device {node}: {error}"))?;
            api.add_node(node, gbm)
                .map_err(|error| format!("EGL device {node}: {error}"))?;
        }
        let mut manager = GpuManager::new(api).map_err(|error| format!("multi-GPU manager: {error}"))?;
        // Enumeration may skip a failed EGL renderer. Validate both identities
        // now, before any client buffers or permanent renderer references exist.
        for (node, node_display_only) in [(render, false), (target, display_only)] {
            let mut renderer = manager
                .single_renderer(&node)
                .map_err(|error| format!("multi-GPU renderer {node}: {error}"))?;
            if !node_display_only {
                if !proves_device(egl_identity(renderer.as_ref()), node, false) {
                    return Err(format!("EGL renderer does not prove the requested device {node}"));
                }
                continue;
            }
            let (identity, name) = display_target_identity(renderer.as_mut());
            if !proves_device(identity, node, true) {
                return Err(format!(
                    "display-only target {node} must be driven by a software renderer (kms_swrast); \
                     its GL renderer is {name:?} and EGL reports {identity:?}"
                ));
            }
            tracing::info!(target = %node, gl_renderer = name.as_deref().unwrap_or("unknown"),
                "display-only KMS target is driven by a software renderer");
        }
        let mut cross = Self {
            manager,
            render,
            target,
            scanout_imports: FormatSet::default(),
            display_only,
        };
        if display_only {
            // Nothing to gain and an unqualified path to lose: a display-only
            // device can only show another GPU's buffer by CPU-copying it in
            // the kernel. Composition still copies every frame into the
            // target's own swapchain, which is the path validated here.
            tracing::info!(render = %render, target = %target,
                "display-only KMS target: no cross-device scanout tranche is probed or advertised");
        } else {
            cross.probe_target_imports(target_fd)?;
        }
        Ok(cross)
    }

    fn probe_target_imports(&mut self, target_fd: DeviceFd) -> Result<(), String> {
        // Equal modifier lists do not prove cross-device import (notably on
        // proprietary NVIDIA). Never steer a client to allocate on the target
        // until the source renderer has accepted a real target allocation.
        let source_formats = self.gles().dmabuf_formats();
        let candidates = self.target_formats();
        let gbm = GbmDevice::new(target_fd).map_err(|error| format!("interop GBM: {error}"))?;
        let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let mut accepted = Vec::new();
        let mut tested = 0;
        for format in candidates
            .into_iter()
            .filter(|format| source_formats.contains(format))
            .take(256)
        {
            tested += 1;
            let Ok(buffer) = allocator.create_buffer(64, 64, format.code, &[format.modifier]) else {
                continue;
            };
            if buffer.format() != format {
                continue;
            }
            let Ok(dma) = buffer.export() else {
                continue;
            };
            if self.gles().import_dmabuf(&dma, None).is_ok() {
                accepted.push(format);
            }
        }
        self.scanout_imports = accepted.into_iter().collect();
        self.gles()
            .cleanup_texture_cache()
            .map_err(|error| format!("interop texture cleanup: {error}"))?;
        tracing::info!(tested, accepted = self.scanout_imports.indexset().len(), render = %self.render, target = %self.target,
            "probed target allocations for cross-GPU scanout feedback");
        Ok(())
    }

    pub fn gles(&mut self) -> &mut GlesRenderer {
        // The manager is private and its device set cannot change after new().
        // GbmGlesBackend only enumerates after mutation, so this cannot fail or
        // replace the context owning live client textures / pending readbacks.
        self.manager
            .devices_mut()
            .expect("fixed GPU set needs no enumeration")
            .find(|device| *device.node() == self.render)
            .expect("validated permanent render GPU")
            .renderer_mut()
    }

    pub fn target_formats(&mut self) -> Vec<Format> {
        self.manager
            .devices_mut()
            .expect("fixed GPU set needs no enumeration")
            .find(|device| *device.node() == self.target)
            .expect("validated permanent target GPU")
            .renderer()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect()
    }

    pub fn renderer(&mut self, format: Fourcc) -> Result<Renderer<'_>, String> {
        self.manager
            .renderer(&self.render, &self.target, format)
            .map_err(|error| format!("multi-GPU frame: {error}"))
    }
}

impl Stack {
    pub fn diagnostics(&self) -> String {
        match self {
            Self::Single(_) => "multi_gpu=disabled".into(),
            Self::Multi(multi) => {
                format!(
                    "multi_gpu=experimental render={} target={} verified_scanout_formats={}{}",
                    multi.render,
                    multi.target,
                    multi.scanout_imports.indexset().len(),
                    if multi.display_only { " target_kind=display-only" } else { "" }
                )
            }
        }
    }

    /// The node client buffers must come from to be offered to KMS for
    /// direct scanout, or `None` to keep every client buffer composited.
    /// A display-only target never scans out another GPU's memory.
    pub fn client_scanout_node(&self, render_node: Option<DrmNode>) -> Option<DrmNode> {
        match self {
            Self::Multi(multi) if multi.display_only => None,
            _ => render_node,
        }
    }

    pub fn gles(&mut self) -> &mut GlesRenderer {
        match self {
            Self::Single(renderer) => renderer,
            Self::Multi(multi) => multi.gles(),
        }
    }
}

// All client textures belong to the permanent render GPU. Delegate their GLES
// drawing while explicitly forwarding damage to MultiFrame's target-copy plan.
// Merely drawing through AsMut<GlesFrame> would omit opaque-element damage and
// could copy an empty/partial target despite having drawn the full source.
impl RenderElement<Renderer<'_>> for SceneElement {
    fn draw(
        &self,
        frame: &mut <Renderer<'_> as RendererSuper>::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Rectangle<i32, Physical>,
        damage: &[Rectangle<i32, Physical>],
        opaque_regions: &[Rectangle<i32, Physical>],
    ) -> Result<(), <Renderer<'_> as RendererSuper>::Error> {
        frame.add_damage(damage.iter().copied().map(|mut rect| {
            rect.loc += dst.loc;
            rect
        }));
        <Self as RenderElement<GlesRenderer>>::draw(self, frame.as_mut(), src, dst, damage, opaque_regions)
            .map_err(Into::into)
    }

    fn underlying_storage(&self, renderer: &mut Renderer<'_>) -> Option<UnderlyingStorage<'_>> {
        <Self as RenderElement<GlesRenderer>>::underlying_storage(self, renderer.as_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::allocator::dmabuf::AsDmabuf;
    use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags};
    use smithay::backend::allocator::Allocator;
    use smithay::backend::renderer::element::{solid::SolidColorRenderElement, Id, Kind};
    use smithay::backend::renderer::utils::CommitCounter;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Renderer as RendererTrait};
    use smithay::utils::{Size, Transform};

    #[test]
    fn a_proven_render_node_stays_the_target_even_beside_a_display_only_primary() {
        assert_eq!(select_target(Some(128), Some((0, NodeType::Primary, false))), Some(128));
    }

    #[test]
    fn a_display_only_primary_names_the_target_when_no_render_node_exists() {
        assert_eq!(select_target(None, Some((0, NodeType::Primary, false))), Some(0));
    }

    #[test]
    fn an_unpaired_kms_device_that_has_a_render_node_is_not_guessed_at() {
        // Its render node exists but was not proven; the primary node must
        // not stand in for it, and startup keeps failing explicitly.
        assert_eq!(select_target(None, Some((1, NodeType::Primary, true))), None);
        assert_eq!(select_target(None, Some((129, NodeType::Render, false))), None);
        assert_eq!(select_target(None, Some((64, NodeType::Control, false))), None);
        assert_eq!(select_target::<u32>(None, None), None);
    }

    #[test]
    fn a_render_node_device_must_be_named_by_egl_exactly() {
        assert!(proves_device(EglIdentity::RenderNode(128), 128, false));
        assert!(!proves_device(EglIdentity::RenderNode(129), 128, false));
        assert!(!proves_device(EglIdentity::Software, 128, false));
        assert!(!proves_device(EglIdentity::Unidentified, 128, false));
    }

    #[test]
    fn only_mesa_cpu_rasterizers_count_as_software_gl_renderers() {
        assert!(is_software_gl_renderer("llvmpipe (LLVM 22.1.8, 128 bits)"));
        assert!(is_software_gl_renderer("softpipe"));
        assert!(!is_software_gl_renderer("zink Vulkan 1.4(Apple M3 Pro (G15S B1) (MESA_HONEYKRISP))"));
        assert!(!is_software_gl_renderer("zink Vulkan 1.3(llvmpipe (LLVM 22.1.8, 256 bits) (MESA_LLVMPIPE))"));
        assert!(!is_software_gl_renderer("NVIDIA GeForce RTX 3090/PCIe/SSE2"));
        assert!(!is_software_gl_renderer(""));
    }

    #[test]
    fn a_display_only_target_must_be_driven_by_a_software_renderer() {
        assert!(proves_device(EglIdentity::Software, 0, true));
        // kmsro: the display fd rendered by some other GPU's render node.
        assert!(!proves_device(EglIdentity::RenderNode(128), 0, true));
        assert!(!proves_device(EglIdentity::RenderNode(0), 0, true));
        assert!(!proves_device(EglIdentity::Unidentified, 0, true));
    }

    #[test]
    #[ignore = "requires two real DRM render nodes in CHONKSTEP_TEST_GPU_PAIR"]
    fn two_real_gpus_preserve_full_and_partial_opaque_pixels_in_both_directions() {
        let pair = std::env::var("CHONKSTEP_TEST_GPU_PAIR").expect("set two comma-separated DRM render nodes");
        let (a, b) = pair.split_once(',').expect("two comma-separated paths");
        assert_ne!(a, b);
        for (render_path, target_path) in [(a, b), (b, a)] {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(target_path)
                .unwrap();
            let target = DrmNode::from_file(&file).unwrap();
            let fd = DeviceFd::from(std::os::fd::OwnedFd::from(file));
            let mut cross = CrossGpu::new(Path::new(render_path), target, fd.clone()).unwrap();
            eprintln!(
                "verified target-to-render import formats: {}",
                cross.scanout_imports.indexset().len()
            );
            let allocator = GbmAllocator::new(GbmDevice::new(fd).unwrap(), GbmBufferFlags::RENDERING);
            let label = format!("{render_path} -> {target_path}");
            verify_transfers(&mut cross, allocator, Fourcc::Abgr8888, &[(96, 64), (5120, 2880)], &label);
        }
    }

    /// The Apple M3 case: composition on a render node, scanout on a KMS
    /// device with no render node (simpledrm on the boot framebuffer). The
    /// KMS fd is opened like any unprivileged client: no DRM master, no
    /// modeset, no framebuffer. Target buffers are allocated the way
    /// `attach_output` allocates the swapchain (target-renderable
    /// modifiers, RENDERING | SCANOUT) and checked through the target's own
    /// CPU renderer, i.e. in the memory KMS would scan out. The Mesa that
    /// runs this must drive the render node natively and the KMS device
    /// with kms_swrast; see docs/gpu-pipeline.md.
    #[test]
    #[ignore = "requires CHONKSTEP_TEST_DISPLAY_ONLY_PAIR=<render node>,<display-only KMS primary>"]
    fn a_render_node_composes_into_a_display_only_kms_target() {
        let pair = std::env::var("CHONKSTEP_TEST_DISPLAY_ONLY_PAIR")
            .expect("set <render node>,<display-only KMS primary>, e.g. /dev/dri/renderD128,/dev/dri/card0");
        let (render_path, kms_path) = pair.split_once(',').expect("two comma-separated paths");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(kms_path)
            .unwrap();
        let kms = DrmNode::from_file(&file).unwrap();
        let target = target_node(None, Some(kms)).expect("the KMS device must have no render node");
        assert_eq!(target, kms);
        let fd = DeviceFd::from(std::os::fd::OwnedFd::from(file));
        let cross = CrossGpu::new(Path::new(render_path), target, fd.clone()).unwrap();
        assert!(cross.display_only);
        assert!(cross.scanout_imports.indexset().is_empty(), "no scanout tranche for a display-only target");
        let stack = Stack::Multi(Box::new(cross));
        assert_eq!(stack.client_scanout_node(Some(kms)), None, "client buffers never reach display-only planes");
        eprintln!("{}", stack.diagnostics());
        let Stack::Multi(mut cross) = stack else { unreachable!() };
        let allocator = GbmAllocator::new(GbmDevice::new(fd).unwrap(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
        let label = format!("{render_path} -> {kms_path}");
        for code in [Fourcc::Xrgb8888, Fourcc::Argb8888] {
            verify_transfers(&mut cross, allocator.clone(), code, &[(96, 64), (3456, 2160)], &label);
        }
    }

    /// Composes opaque full-frame and partial solid colors on the render
    /// GPU into target buffers of `code`, without a clear, and reads the
    /// target back. Every frame must cross devices (DMA or CPU copy).
    fn verify_transfers(
        cross: &mut CrossGpu,
        mut allocator: GbmAllocator<DeviceFd>,
        code: Fourcc,
        sizes: &[(u32, u32)],
        label: &str,
    ) {
        let modifiers = cross
            .target_formats()
            .into_iter()
            .filter(|format| format.code == code)
            .map(|format| format.modifier)
            .collect::<Vec<_>>();
        assert!(!modifiers.is_empty(), "target must render {code:?}");
        eprintln!("{label}: target renderable {code:?} modifiers: {modifiers:?}");
        let before = smithay::backend::renderer::multigpu::copy_stats();
        for &(width, height) in sizes {
            let buffer = allocator.create_buffer(width, height, code, &modifiers).unwrap();
            let mut dma = buffer.export().unwrap();
            let extent = Size::<i32, Physical>::from((width as i32, height as i32));
            let whole = Rectangle::from_size(extent);
            for partial in [false, true] {
                let dst = if partial {
                    Rectangle::new((13, 17).into(), (31, 23).into())
                } else {
                    whole
                };
                let color = if partial { [192, 32, 64] } else { [32, 64, 128] };
                let element: SceneElement = SolidColorRenderElement::new(
                    Id::new(),
                    dst,
                    CommitCounter::default(),
                    Color32F::new(
                        color[0] as f32 / 255.0,
                        color[1] as f32 / 255.0,
                        color[2] as f32 / 255.0,
                        1.0,
                    ),
                    Kind::Unspecified,
                )
                .into();
                let mut renderer = cross.renderer(code).unwrap();
                let mut fb = renderer.bind(&mut dma).unwrap();
                let mut frame = renderer.render(&mut fb, extent, Transform::Normal).unwrap();
                // No clear: all damage is opaque custom GLES drawing. This
                // fails if damage forwarding to the transfer is omitted.
                <SceneElement as RenderElement<Renderer<'_>>>::draw(
                    &element,
                    &mut frame,
                    smithay::backend::renderer::element::Element::src(&element),
                    dst,
                    &[Rectangle::from_size(dst.size)],
                    &[Rectangle::from_size(dst.size)],
                )
                .unwrap();
                let sync = frame.finish().unwrap();
                // This is the dedicated hardware test thread, not the
                // compositor event loop: finish the target GPU before QA.
                sync.wait().unwrap();
                let mapping = renderer
                    .copy_framebuffer(
                        &fb,
                        Rectangle::from_size((width as i32, height as i32).into()),
                        Fourcc::Abgr8888,
                    )
                    .unwrap();
                let pixels = renderer.map_texture(&mapping).unwrap();
                assert_eq!(pixels.len(), width as usize * height as usize * 4);
                for (x, y) in [(0, 0), (15, 19), (width as usize - 1, height as usize - 1)] {
                    let expected = if partial && x == 15 && y == 19 {
                        [192, 32, 64, 255]
                    } else {
                        [32, 64, 128, 255]
                    };
                    let at = (y * width as usize + x) * 4;
                    assert_eq!(
                        &pixels[at..at + 4],
                        &expected,
                        "{label}, {code:?}, {width}x{height}, partial={partial}, pixel={x},{y}"
                    );
                }
            }
        }
        let after = smithay::backend::renderer::multigpu::copy_stats();
        let dma = after.dma_frames - before.dma_frames;
        let cpu = after.cpu_frames - before.cpu_frames;
        assert_eq!(dma + cpu, 2 * sizes.len() as u64, "every rendered frame must cross to its target");
        eprintln!(
            "verified {label} {code:?}: dma_copies={dma}, cpu_copies={cpu}, cpu_pixels={}",
            after.cpu_pixels - before.cpu_pixels
        );
    }
}
