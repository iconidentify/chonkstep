//! Explicit render-GPU / target-GPU composition. Single-device sessions keep
//! their original GLES renderer; cross-device copies are opt-in at startup.
use std::path::Path;

use smithay::backend::allocator::dmabuf::AsDmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Allocator, Buffer as AllocatedBuffer, Format, Fourcc};
use smithay::backend::drm::{DrmNode, NodeType};
use smithay::backend::renderer::element::{RenderElement, UnderlyingStorage};
use smithay::backend::renderer::gles::GlesRenderer;
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
        for node in [render, target] {
            let renderer = manager
                .single_renderer(&node)
                .map_err(|error| format!("multi-GPU renderer {node}: {error}"))?;
            if crate::dmabuf::render_node_for_renderer(renderer.as_ref()) != Some(node) {
                return Err(format!("EGL renderer does not prove the requested device {node}"));
            }
        }
        let mut cross = Self {
            manager,
            render,
            target,
            scanout_imports: FormatSet::default(),
        };
        cross.probe_target_imports(target_fd)?;
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
                    "multi_gpu=experimental render={} target={} verified_scanout_formats={}",
                    multi.render, multi.target, multi.scanout_imports.indexset().len()
                )
            }
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
impl RenderElement<Renderer<'_>> for SceneElement<GlesRenderer> {
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
            let mut allocator = GbmAllocator::new(GbmDevice::new(fd).unwrap(), GbmBufferFlags::RENDERING);
            let modifiers = cross
                .target_formats()
                .into_iter()
                .filter(|format| format.code == Fourcc::Abgr8888)
                .map(|format| format.modifier)
                .collect::<Vec<_>>();
            assert!(!modifiers.is_empty(), "target must render ABGR8888");
            eprintln!("target {target_path} renderable modifiers: {modifiers:?}");
            let before = smithay::backend::renderer::multigpu::copy_stats();
            for (width, height) in [(96, 64), (5120, 2880)] {
                let buffer = allocator
                    .create_buffer(width, height, Fourcc::Abgr8888, &modifiers)
                    .unwrap();
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
                    let element: SceneElement<GlesRenderer> = SolidColorRenderElement::new(
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
                    let mut renderer = cross.renderer(Fourcc::Abgr8888).unwrap();
                    let mut fb = renderer.bind(&mut dma).unwrap();
                    let mut frame = renderer.render(&mut fb, extent, Transform::Normal).unwrap();
                    // No clear: all damage is opaque custom GLES drawing. This
                    // fails if damage forwarding to the transfer is omitted.
                    <SceneElement<GlesRenderer> as RenderElement<Renderer<'_>>>::draw(
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
                            "{render_path} -> {target_path}, {width}x{height}, partial={partial}, pixel={x},{y}"
                        );
                    }
                }
            }
            let after = smithay::backend::renderer::multigpu::copy_stats();
            let dma = after.dma_frames - before.dma_frames;
            let cpu = after.cpu_frames - before.cpu_frames;
            assert_eq!(dma + cpu, 4, "every rendered frame must cross to its target");
            eprintln!(
                "verified {render_path} -> {target_path}: dma_copies={dma}, cpu_copies={cpu}, cpu_pixels={}",
                after.cpu_pixels - before.cpu_pixels
            );
        }
    }
}
