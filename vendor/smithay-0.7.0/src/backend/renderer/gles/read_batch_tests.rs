use super::*;
use crate::backend::egl::{native::EGLSurfacelessDisplay, EGLDisplay};
use std::panic::{catch_unwind, AssertUnwindSafe};

fn paint(
    frame: &mut GlesFrame<'_, '_>,
    texture: &GlesTexture,
    destination: Rectangle<i32, Physical>,
) -> Result<(), GlesError> {
    frame.render_texture_from_to(
        texture,
        Rectangle::from_size((8.0, 8.0).into()),
        destination,
        &[Rectangle::from_size(destination.size)],
        &[],
        Transform::Normal,
        1.0,
        None,
        &[],
    )
}

#[test]
#[ignore = "requires native GLES/EGL; checks scoped mutable texture synchronization"]
fn scoped_texture_reads_hold_locks_and_recover_from_errors_and_unwind() {
    for disable_fencing in [false, true] {
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        // Keep a sharing context alive so disabling fences exercises the
        // original shared-context Finish fallback, rather than no-op cleanup.
        let _shared = EGLContext::new_shared(&display, &context).unwrap();
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        assert!(
            renderer.capabilities.contains(&Capability::Fencing),
            "native gate needs fence support to exercise both synchronization paths"
        );
        if disable_fencing {
            renderer.capabilities.retain(|cap| *cap != Capability::Fencing);
        }
        let red = renderer
            .import_memory(
                &[255, 0, 0, 255].repeat(64),
                Fourcc::Abgr8888,
                (8, 8).into(),
                false,
            )
            .unwrap();
        let green = renderer
            .import_memory(
                &[0, 255, 0, 255].repeat(64),
                Fourcc::Abgr8888,
                (8, 8).into(),
                false,
            )
            .unwrap();
        let mut output: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (16, 8).into()).unwrap();
        let mut target = renderer.bind(&mut output).unwrap();
        let mut frame = renderer
            .render(&mut target, (16, 8).into(), Transform::Normal)
            .unwrap();
        frame
            .clear(Color32F::TRANSPARENT, &[Rectangle::from_size((16, 8).into())])
            .unwrap();

        // A mixed-texture attempt must fail before waiting for this write lock.
        // This deterministically checks the lock-order boundary without races.
        let other_writer = green.0.sync.write().unwrap();
        frame
            .with_texture_read(&red, |frame| {
                assert!(red.0.sync.try_write().is_err());
                paint(frame, &red, Rectangle::new((0, 0).into(), (8, 4).into()))?;
                frame.with_texture_read(&red, |frame| {
                    assert!(red.0.sync.try_write().is_err());
                    paint(frame, &red, Rectangle::new((0, 4).into(), (8, 4).into()))
                })?;
                assert!(matches!(
                    frame.with_texture_read(&green, |_| -> Result<(), GlesError> {
                        panic!("a conflicting callback must not run")
                    }),
                    Err(GlesError::TextureReadBatchConflict)
                ));
                assert!(matches!(
                    paint(frame, &green, Rectangle::new((12, 0).into(), (4, 8).into())),
                    Err(GlesError::TextureReadBatchConflict)
                ));
                assert!(red.0.sync.try_write().is_err());
                Ok(())
            })
            .unwrap();
        drop(other_writer);
        assert!(frame.texture_read_batch.is_none());
        assert!(red.0.sync.try_write().is_ok());

        let error: Result<(), GlesError> = frame.with_texture_read(&red, |frame| {
            paint(frame, &red, Rectangle::new((8, 0).into(), (4, 4).into()))?;
            Err(GlesError::UnexpectedSize)
        });
        assert!(matches!(error, Err(GlesError::UnexpectedSize)));
        assert!(frame.texture_read_batch.is_none());
        assert!(red.0.sync.try_write().is_ok());

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _: Result<(), GlesError> = frame.with_texture_read(&red, |frame| {
                paint(frame, &red, Rectangle::new((8, 4).into(), (4, 4).into()))?;
                panic!("exercise read-scope cleanup after submitted work")
            });
        }));
        assert!(panic.is_err());
        assert!(frame.texture_read_batch.is_none());
        assert!(red.0.sync.try_write().is_ok());
        // Ordinary drawing resumes after both failure exits.
        paint(&mut frame, &green, Rectangle::new((12, 0).into(), (4, 8).into())).unwrap();
        frame.finish().unwrap().wait().unwrap();
        let mapping = renderer
            .copy_framebuffer(&target, Rectangle::from_size((16, 8).into()), Fourcc::Abgr8888)
            .unwrap();
        let pixels = renderer.map_texture(&mapping).unwrap();
        for y in 0..8 {
            for x in 0..16 {
                let expected = if x < 12 {
                    [255, 0, 0, 255]
                } else {
                    [0, 255, 0, 255]
                };
                assert_eq!(&pixels[(y * 16 + x) * 4..(y * 16 + x + 1) * 4], &expected);
            }
        }
    }
}

#[test]
#[ignore = "requires native GLES/EGL; checks custom pixel vertex interfaces"]
fn custom_pixel_vertices_validate_inputs_and_preserve_default_drawing() {
    let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
    let context = EGLContext::new(&display).unwrap();
    let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
    let fragment = "precision highp float; varying vec2 v_coords;\n\
        void main() { gl_FragColor=vec4(v_coords,0.0,1.0); }";
    let vertex = "#version 100\n\
        attribute vec2 vert; attribute vec4 vert_position; uniform mat3 matrix;\n\
        varying vec2 v_coords;\n\
        void main() { vec2 p=vert*vert_position.zw+vert_position.xy;\n\
        v_coords=p/8.0; gl_Position=vec4(matrix*vec3(p,1.0),1.0); }";
    let default = renderer.compile_custom_pixel_shader(fragment, &[]).unwrap();
    // No tex_matrix is needed by the custom stage; optimized-away optional
    // uniforms must remain valid. Additional vertex uniforms are covered by the
    // compositor's nine-slice reference-pixel gate.
    let custom = renderer
        .compile_custom_pixel_shader_with_vertex(vertex, fragment, &[])
        .unwrap();
    for (bad_vertex, expected) in [
        (
            vertex.replace("vert*vert_position.zw", "vec2(0.5)*vert_position.zw"),
            "vert",
        ),
        (
            vertex.replace("vert*vert_position.zw+vert_position.xy", "vert*8.0"),
            "vert_position",
        ),
        (vertex.replace("matrix*vec3(p,1.0)", "vec3(p,1.0)"), "matrix"),
        (
            vertex
                .replace("vec2 vert;", "vec3 vert;")
                .replace("vert*vert_position", "vert.xy*vert_position"),
            "vert",
        ),
        (
            vertex
                .replace("mat3 matrix;", "mat4 matrix;")
                .replace("vec4(matrix*vec3(p,1.0),1.0)", "matrix*vec4(p,0.0,1.0)"),
            "matrix",
        ),
    ] {
        assert!(matches!(
            renderer.compile_custom_pixel_shader_with_vertex(bad_vertex, fragment, &[]),
            Err(GlesError::InvalidPixelShaderInterface(name)) if name == expected
        ));
    }
    let debug_failure =
        format!("{fragment}\n#ifdef DEBUG_FLAGS\n#error deliberate debug compile failure\n#endif");
    assert!(matches!(
        renderer.compile_custom_pixel_shader_with_vertex(vertex, debug_failure, &[]),
        Err(GlesError::ShaderCompileError)
    ));

    let mut output: GlesTexture = renderer.create_buffer(Fourcc::Abgr8888, (16, 8).into()).unwrap();
    let mut target = renderer.bind(&mut output).unwrap();
    let mut frame = renderer
        .render(&mut target, (16, 8).into(), Transform::Normal)
        .unwrap();
    frame
        .clear(Color32F::TRANSPARENT, &[Rectangle::from_size((16, 8).into())])
        .unwrap();
    let damage = [
        Rectangle::new((0, 0).into(), (3, 8).into()),
        Rectangle::new((3, 0).into(), (5, 8).into()),
    ];
    for (program, x) in [(&default, 0), (&custom, 8)] {
        frame
            .render_pixel_shader_to(
                program,
                Rectangle::from_size((8.0, 8.0).into()),
                Rectangle::new((x, 0).into(), (8, 8).into()),
                (8, 8).into(),
                Some(&damage),
                1.0,
                &[],
            )
            .unwrap();
    }
    frame.finish().unwrap().wait().unwrap();
    let mapping = renderer
        .copy_framebuffer(&target, Rectangle::from_size((16, 8).into()), Fourcc::Abgr8888)
        .unwrap();
    let pixels = renderer.map_texture(&mapping).unwrap();
    for y in 0..8 {
        for x in 0..8 {
            let left = (y * 16 + x) * 4;
            let right = left + 8 * 4;
            assert_eq!(&pixels[left..left + 4], &pixels[right..right + 4]);
            assert_eq!(pixels[left + 3], 255);
        }
    }
    assert_eq!(unsafe { renderer.gl.GetError() }, ffi::NO_ERROR);
}
