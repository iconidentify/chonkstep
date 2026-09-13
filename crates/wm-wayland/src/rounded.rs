//! Rounded frame silhouettes over live textures: no offscreen window image.
use crate::renderer::SceneElement;
use smithay::backend::renderer::{
    element::{Element, Id, Kind, RenderElement},
    gles::{
        GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, GlesTexture, Uniform,
        UniformName, UniformType,
    },
    utils::{CommitCounter, DamageSet, OpaqueRegions},
    Color32F,
};
use smithay::utils::{Buffer, Physical, Rectangle, Scale, Transform};
use std::{cell::RefCell, rc::Rc};
use wm_theme_api::{DecorationShape, Point, Rect};
type PixelRect = Rectangle<i32, Physical>;

/// Full-width middle first: ordinary clients start below the title corners and
/// therefore need only the middle and bottom strip, without changing coverage.
fn covered_centers(rect: PixelRect, radius: i32) -> [PixelRect; 3] {
    [
        Rectangle::new(
            (rect.loc.x, rect.loc.y + radius).into(),
            (rect.size.w, rect.size.h - 2 * radius).into(),
        ),
        Rectangle::new(
            (rect.loc.x + radius, rect.loc.y).into(),
            (rect.size.w - 2 * radius, radius).into(),
        ),
        Rectangle::new(
            (rect.loc.x + radius, rect.loc.y + rect.size.h - radius).into(),
            (rect.size.w - 2 * radius, radius).into(),
        ),
    ]
}

/// Test only newly emitted client-tree surfaces, before applying a rounded
/// mask. Popups, borders, and other planes must stay outside this slice.
/// One full opaque rectangle proves the resize fill redundant. More complex
/// declarations conservatively retain it and avoid allocating opacity copies.
pub(crate) fn opaque_client_covers(elements: &[SceneElement], requested: Rect) -> bool {
    elements.iter().any(|element| match element {
        SceneElement::Surface(surface) => {
            opaque_element_covers(surface, surface.element().opaque_region_count(), requested)
        }
        _ => false,
    })
}

fn opaque_element_covers<E: Element>(element: &E, region_count: usize, requested: Rect) -> bool {
    if region_count != 1 || element.alpha() != 1.0 || requested.size.w == 0 || requested.size.h == 0
    {
        return false;
    }
    let geometry = element.geometry(1.0.into());
    // Keep translated endpoints wide: valid surface origins and opacity can
    // individually fit i32 while their sum does not.
    let covers = |rect: PixelRect, offset: (i64, i64)| {
        let x = i64::from(rect.loc.x) + offset.0;
        let y = i64::from(rect.loc.y) + offset.1;
        rect.size.w > 0
            && rect.size.h > 0
            && x <= i64::from(requested.pos.x)
            && y <= i64::from(requested.pos.y)
            && x + i64::from(rect.size.w)
                >= i64::from(requested.pos.x) + i64::from(requested.size.w)
            && y + i64::from(rect.size.h)
                >= i64::from(requested.pos.y) + i64::from(requested.size.h)
    };
    covers(geometry, (0, 0))
        && element.opaque_regions(1.0.into()).iter().any(|opaque| {
            covers(
                *opaque,
                (i64::from(geometry.loc.x), i64::from(geometry.loc.y)),
            )
        })
}

/// Convert a physical rounded-box rectangle to framebuffer coordinates once
/// per draw. Its silhouette and radius are invariant under signed axis swaps;
/// scale, shear, arbitrary rotation, and invalid projections retain the caller's
/// affine shader path. Small floating-point projection roundoff is tolerated.
pub(crate) fn framebuffer_rect(affine: [[f32; 3]; 2], rect: [f32; 4]) -> Option<[f32; 4]> {
    if !affine
        .iter()
        .flatten()
        .chain(rect.iter())
        .all(|v| v.is_finite())
        || rect[2] < 0.0
        || rect[3] < 0.0
    {
        return None;
    }
    let [[xx, xy, tx], [yx, yy, ty]] = affine.map(|row| row.map(f64::from));
    let near_zero = |v: f64| v == 0.0;
    let near_unit = |v: f64| (v.abs() - 1.0).abs() <= 1.0e-6;
    if !((near_unit(xx) && near_zero(xy) && near_zero(yx) && near_unit(yy))
        || (near_zero(xx) && near_unit(xy) && near_unit(yx) && near_zero(yy)))
    {
        return None;
    }
    let determinant = xx * yy - xy * yx;
    let [left, top, width, height] = rect.map(f64::from);
    let mut minimum = [f64::INFINITY; 2];
    let mut maximum = [f64::NEG_INFINITY; 2];
    for (x, y) in [
        (left, top),
        (left + width, top),
        (left, top + height),
        (left + width, top + height),
    ] {
        let (x, y) = (x - tx, y - ty);
        let point = [
            (yy * x - xy * y) / determinant,
            (-yx * x + xx * y) / determinant,
        ];
        for axis in 0..2 {
            minimum[axis] = minimum[axis].min(point[axis]);
            maximum[axis] = maximum[axis].max(point[axis]);
        }
    }
    let converted = [
        minimum[0],
        minimum[1],
        maximum[0] - minimum[0],
        maximum[1] - minimum[1],
    ]
    .map(|v| v as f32);
    converted.iter().all(|v| v.is_finite()).then_some(converted)
}

#[derive(Debug)]
struct Programs {
    texture: GlesTexProgram,
    solid: GlesPixelProgram,
    uniforms: RefCell<Vec<Uniform<'static>>>,
}
#[derive(Debug)]
struct ProgramCache(Option<Rc<Programs>>);

fn programs(renderer: &mut GlesRenderer) -> Option<Rc<Programs>> {
    if let Some(cache) = renderer.egl_context().user_data().get::<ProgramCache>() {
        return cache.0.clone();
    }
    let result = (|| {
        let common = [
            UniformName::new("physical_x", UniformType::_3f),
            UniformName::new("physical_y", UniformType::_3f),
            UniformName::new("clip_rect", UniformType::_4f),
            UniformName::new("radius", UniformType::_1f),
        ];
        let mut texture_names = common.to_vec();
        texture_names.push(UniformName::new("framebuffer_space", UniformType::_1f));
        let texture = renderer.compile_custom_texture_shader(TEXTURE_SHADER, &texture_names)?;
        let mut solid_names = common.to_vec();
        solid_names.extend([
            UniformName::new("fill", UniformType::_4f),
            UniformName::new("border", UniformType::_1f),
        ]);
        let solid = renderer.compile_custom_pixel_shader(SOLID_SHADER, &solid_names)?;
        Ok::<_, GlesError>(Rc::new(Programs {
            texture,
            solid,
            uniforms: RefCell::new(Vec::with_capacity(5)),
        }))
    })()
    .map_err(|error| tracing::warn!(?error, "rounded frame shader unavailable"))
    .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| ProgramCache(result.clone()));
    result
}

#[derive(Debug)]
pub(crate) struct Rounded<E> {
    inner: E,
    programs: Rc<Programs>,
    rect: PixelRect,
    radius: f32,
    color: Option<Color32F>,
    texture: Option<GlesTexture>,
    border: f32,
}

impl<E: Element> Element for Rounded<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> PixelRect {
        self.inner.geometry(scale)
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, Physical> {
        if self.border > 0.0 {
            return OpaqueRegions::default();
        }
        let geometry = self.geometry(scale);
        let r = self.radius.ceil() as i32;
        let rect = self.rect;
        // Disjoint solid centers omit all four corner squares. A client
        // with pathological opacity fragmentation gets conservative opacity,
        // keeping this metadata within Smithay's sixteen inline rectangles.
        let centers = covered_centers(rect, r);
        let mut regions = [PixelRect::default(); 16];
        let mut len = 0;
        for mut opaque in self.inner.opaque_regions(scale) {
            opaque.loc += geometry.loc;
            for center in centers {
                if let Some(mut piece) = opaque.intersection(center) {
                    if len == regions.len() {
                        return OpaqueRegions::default();
                    }
                    piece.loc -= geometry.loc;
                    regions[len] = piece;
                    len += 1;
                }
            }
        }
        regions[..len].iter().copied().collect()
    }
}

impl<E: RenderElement<GlesRenderer>> RenderElement<GlesRenderer> for Rounded<E> {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: PixelRect,
        damage: &[PixelRect],
        opaque: &[PixelRect],
    ) -> Result<(), GlesError> {
        if let Some(texture) = &self.texture {
            frame.with_texture_read(texture, |frame| {
                self.draw_parts(frame, src, dst, damage, opaque)
            })
        } else {
            self.draw_parts(frame, src, dst, damage, opaque)
        }
    }
    // A masked texture cannot use direct scanout: its transparent corners need
    // the underlying scene. Fully contained content never gets this wrapper.
}

impl<E: RenderElement<GlesRenderer>> Rounded<E> {
    fn draw_parts(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: PixelRect,
        damage: &[PixelRect],
        opaque: &[PixelRect],
    ) -> Result<(), GlesError> {
        let geometry = self.inner.geometry(1.0.into());
        let base = self.inner.src();
        let scale = base.size
            / self
                .inner
                .transform()
                .invert()
                .transform_size(geometry.size)
                .to_f64();
        let mut crop = src;
        crop.loc -= base.loc;
        let cropped = crop
            .to_logical(scale, self.inner.transform(), &base.size)
            .to_physical_precise_round(1.0);
        let origin = geometry.loc + cropped.loc;
        let shift = dst.loc - origin;
        let rect = Rectangle::new(self.rect.loc + shift, self.rect.size);
        let [[xx, xy, xz], [yx, yy, yz]] = frame.framebuffer_to_physical();
        let physical = [
            rect.loc.x as f32,
            rect.loc.y as f32,
            rect.size.w as f32,
            rect.size.h as f32,
        ];
        let framebuffer = self
            .color
            .is_none()
            .then(|| framebuffer_rect([[xx, xy, xz], [yx, yy, yz]], physical))
            .flatten();
        let [x, y, w, h] = framebuffer.unwrap_or(physical);
        let uniforms = [
            Uniform::new("physical_x", (xx, xy, xz)),
            Uniform::new("physical_y", (yx, yy, yz)),
            Uniform::new("clip_rect", (x, y, w, h)),
            Uniform::new("radius", self.radius),
            Uniform::new(
                "framebuffer_space",
                if framebuffer.is_some() { 1.0f32 } else { 0.0 },
            ),
        ];
        // Keep the normal texture shader and opacity optimization for the
        // client body. Only the tiny corner squares need the rounded shader;
        // the enclosing read scope synchronizes all these draws together.
        if self.border > 0.0 {
            return self.draw_masked(frame, src, dst, damage, opaque, &uniforms);
        }
        let r = self.radius.ceil() as i32;
        let centers = covered_centers(rect, r);
        let corners = [
            (rect.loc.x, rect.loc.y),
            (rect.loc.x + rect.size.w - r, rect.loc.y),
            (rect.loc.x, rect.loc.y + rect.size.h - r),
            (rect.loc.x + rect.size.w - r, rect.loc.y + rect.size.h - r),
        ]
        .map(|p| Rectangle::new(p.into(), (r, r).into()));
        // Stack scratch storage remains bounded even with fragmented damage.
        // The texture read scope surrounds every chunk, so its final fence
        // covers both the ordinary body and all masked corners.
        for chunk in damage.chunks(8) {
            let mut body = [PixelRect::default(); 24];
            let mut body_len = 0;
            let mut edge = [PixelRect::default(); 32];
            let mut edge_len = 0;
            for part in chunk {
                let absolute = Rectangle::new(part.loc + dst.loc, part.size);
                for center in centers {
                    if let Some(mut piece) = absolute.intersection(center) {
                        piece.loc -= dst.loc;
                        body[body_len] = piece;
                        body_len += 1;
                    }
                }
                for corner in corners {
                    if let Some(mut piece) = absolute.intersection(corner) {
                        piece.loc -= dst.loc;
                        edge[edge_len] = piece;
                        edge_len += 1;
                    }
                }
            }
            if body_len > 0 {
                self.inner
                    .draw(frame, src, dst, &body[..body_len], opaque)?;
            }
            if edge_len > 0 {
                self.draw_masked(frame, src, dst, &edge[..edge_len], opaque, &uniforms)?;
            }
        }
        Ok(())
    }

    fn draw_masked(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: PixelRect,
        damage: &[PixelRect],
        _opaque: &[PixelRect],
        uniforms: &[Uniform<'static>; 5],
    ) -> Result<(), GlesError> {
        if let Some(color) = self.color {
            let additional = [
                uniforms[0].clone(),
                uniforms[1].clone(),
                uniforms[2].clone(),
                uniforms[3].clone(),
                Uniform::new("fill", (color.r(), color.g(), color.b(), color.a())),
                Uniform::new("border", self.border),
            ];
            return frame.render_pixel_shader_to(
                &self.programs.solid,
                Rectangle::from_size((dst.size.w as f64, dst.size.h as f64).into()),
                dst,
                (dst.size.w, dst.size.h).into(),
                Some(damage),
                1.0,
                &additional,
            );
        }
        let previous = frame.take_tex_program_override();
        let mut storage = self.programs.uniforms.borrow_mut();
        storage.clear();
        storage.extend(uniforms.iter().cloned());
        frame.override_default_tex_program(
            self.programs.texture.clone(),
            std::mem::take(&mut *storage),
        );
        // Scene opacity still hides covered content, and the ordinary body
        // retains its draw-time opacity optimization. These corner fragments
        // need blending; do not split them into additional opaque draws.
        let result = self.inner.draw(frame, src, dst, damage, &[]);
        if let Some((_, returned)) = frame.take_tex_program_override() {
            *storage = returned;
        }
        if let Some((program, uniforms)) = previous {
            frame.override_default_tex_program(program, uniforms);
        }
        result
    }
}

fn pixel_rect(rect: Rect) -> PixelRect {
    Rectangle::new(
        (rect.pos.x, rect.pos.y).into(),
        (rect.size.w as i32, rect.size.h as i32).into(),
    )
}

/// Mask freshly appended content or opaque perimeter solids. Already rounded
/// raster title corners keep their original coverage; popups are emitted before
/// callers take `start` and therefore remain outside the frame silhouette.
pub(crate) fn mask_plane(
    elements: &mut [SceneElement],
    start: usize,
    renderer: &mut GlesRenderer,
    shape: Option<DecorationShape>,
    inner: bool,
) {
    mask_plane_inner(elements, start, renderer, shape, inner, false);
}

/// An emitted lower ring owns its corner squares. Keep that fact explicit:
/// shaded frames and shader-unavailable fallbacks retain their straight strips.
pub(crate) fn mask_frame_solids(
    elements: &mut [SceneElement],
    start: usize,
    renderer: &mut GlesRenderer,
    shape: Option<DecorationShape>,
    lower_border_drawn: bool,
) {
    mask_plane_inner(elements, start, renderer, shape, false, lower_border_drawn);
}

// Recognize only matching, narrow straight border strips. Broad fills, title
// rows, custom colors and unrelated solid geometry keep their existing mask.
// Each strip remains one rectangle, preserving its retained element identity.
fn trim_lower_border_strip(
    geometry: PixelRect,
    color: Color32F,
    shape: DecorationShape,
) -> Option<PixelRect> {
    if shape.radius == 0 || shape.border == 0 {
        return None;
    }
    let expected = shape.border_rgb.map(|c| f32::from(c) / 255.0 * color.a());
    if [color.r(), color.g(), color.b()] != expected {
        return None;
    }
    let [left, top] = [i64::from(shape.rect.pos.x), i64::from(shape.rect.pos.y)];
    let [right, bottom] = [
        left + i64::from(shape.rect.size.w),
        top + i64::from(shape.rect.size.h),
    ];
    let [mut x0, y0] = [i64::from(geometry.loc.x), i64::from(geometry.loc.y)];
    let [mut x1, mut y1] = [
        x0 + i64::from(geometry.size.w),
        y0 + i64::from(geometry.size.h),
    ];
    let border = i64::from(shape.border);
    let radius = i64::from(shape.radius);
    if x0 < left || x1 > right || y0 < top || y1 > bottom {
        return None;
    }
    if y1 == bottom && y0 >= bottom - border {
        x0 = x0.max(left + radius);
        x1 = x1.min(right - radius).max(x0);
    } else if x1 - x0 <= border && (x0 == left || x1 == right) {
        y1 = y1.min(bottom - radius).max(y0);
    } else {
        return None;
    }
    Some(PixelRect::new(
        (i32::try_from(x0).ok()?, i32::try_from(y0).ok()?).into(),
        (i32::try_from(x1 - x0).ok()?, i32::try_from(y1 - y0).ok()?).into(),
    ))
}

fn mask_plane_inner(
    elements: &mut [SceneElement],
    start: usize,
    renderer: &mut GlesRenderer,
    shape: Option<DecorationShape>,
    inner: bool,
    lower_border_drawn: bool,
) {
    if start >= elements.len() {
        return;
    }
    let Some(mut shape) = shape else {
        return;
    };
    shape = shape.normalized();
    if shape.radius == 0 {
        return;
    }
    if inner {
        let b = u32::from(shape.border)
            .min(shape.rect.size.w / 2)
            .min(shape.rect.size.h / 2);
        shape.rect.pos.x += b as i32;
        shape.rect.pos.y += b as i32;
        shape.rect.size.w -= 2 * b;
        shape.rect.size.h -= 2 * b;
        shape.radius = shape.radius.saturating_sub(b as u16);
    }
    let rect = pixel_rect(shape.rect);
    let radius = f32::from(shape.radius);
    let Some(programs) = programs(renderer) else {
        return;
    };
    let empty = smithay::backend::renderer::element::solid::SolidColorRenderElement::new(
        elements
            .get(start)
            .map(|e| e.id().clone())
            .unwrap_or_else(Id::new),
        PixelRect::default(),
        CommitCounter::default(),
        Color32F::TRANSPARENT,
        Kind::Unspecified,
    );
    for element in &mut elements[start..] {
        if lower_border_drawn {
            if let SceneElement::Solid(solid) = element {
                if let Some(geometry) =
                    trim_lower_border_strip(solid.geometry(1.0.into()), solid.color(), shape)
                {
                    *solid =
                        smithay::backend::renderer::element::solid::SolidColorRenderElement::new(
                            solid.id().clone(),
                            geometry,
                            solid.current_commit(),
                            solid.color(),
                            solid.kind(),
                        );
                    if geometry.is_empty() {
                        continue;
                    }
                }
            }
        }
        let geometry = element.geometry(1.0.into());
        let center = Rectangle::new(
            (rect.loc.x, rect.loc.y + shape.radius as i32).into(),
            (rect.size.w, rect.size.h - i32::from(shape.radius) * 2).into(),
        );
        if center.contains_rect(geometry) {
            continue;
        }
        let old = std::mem::replace(element, empty.clone().into());
        let wrap = |inner, color| Rounded {
            inner,
            programs: programs.clone(),
            rect,
            radius,
            color,
            texture: None,
            border: 0.0,
        };
        *element = match old {
            SceneElement::Surface(surface) => {
                use smithay::backend::renderer::element::surface::WaylandSurfaceTexture;
                let (color, texture) = match surface.element().texture() {
                    WaylandSurfaceTexture::SolidColor(color) => {
                        (Some(*color * surface.alpha()), None)
                    }
                    WaylandSurfaceTexture::Texture(texture) => (None, Some(texture.clone())),
                };
                Rounded {
                    inner: surface,
                    programs: programs.clone(),
                    rect,
                    radius,
                    color,
                    texture,
                    border: 0.0,
                }
                .into()
            }
            SceneElement::Solid(solid) => {
                let color = solid.color();
                wrap(solid, Some(color)).into()
            }
            other => other,
        };
    }
}

pub(crate) fn translated_shape(
    shape: Option<DecorationShape>,
    frame: Point,
    source: Point,
    destination: Point,
    sx: f64,
    sy: f64,
) -> Option<DecorationShape> {
    shape.map(|mut shape| {
        shape.rect =
            crate::overview::scaled_chrome_rect(shape.rect, frame, source, destination, sx, sy);
        shape.radius = (f64::from(shape.radius) * sx.min(sy)).round() as u16;
        shape.border = (f64::from(shape.border) * sx.min(sy)).round() as u16;
        shape.normalized()
    })
}

pub(crate) fn push_border(
    elements: &mut Vec<SceneElement>,
    renderer: &mut GlesRenderer,
    shape: Option<DecorationShape>,
    ids: &[Id; 2],
    commit: CommitCounter,
    alpha: f32,
) -> bool {
    let Some(shape) = shape
        .map(DecorationShape::normalized)
        .filter(|s| s.radius > 0 && s.border > 0)
    else {
        return false;
    };
    let Some(programs) = programs(renderer) else {
        return false;
    };
    let r = i32::from(shape.radius);
    let rect = pixel_rect(shape.rect);
    let [red, green, blue] = shape.border_rgb.map(|c| f32::from(c) / 255.0 * alpha);
    let color = Color32F::new(red, green, blue, alpha);
    for (index, x) in [rect.loc.x, rect.loc.x + rect.size.w - r]
        .into_iter()
        .enumerate()
    {
        let solid = smithay::backend::renderer::element::solid::SolidColorRenderElement::new(
            ids[index].clone(),
            Rectangle::new((x, rect.loc.y + rect.size.h - r).into(), (r, r).into()),
            commit,
            color,
            Kind::Unspecified,
        );
        elements.push(
            Rounded {
                inner: solid,
                programs: programs.clone(),
                rect,
                radius: r as f32,
                color: Some(color),
                texture: None,
                border: f32::from(shape.border),
            }
            .into(),
        );
    }
    true
}

const TEXTURE_SHADER: &str = r#"
#version 100
//_DEFINES_
#ifdef EXTERNAL
#extension GL_OES_EGL_image_external : require
#endif
precision highp float;
#ifdef EXTERNAL
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
uniform vec3 physical_x;
uniform vec3 physical_y;
uniform vec4 clip_rect;
uniform float radius;
uniform float framebuffer_space;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
void main() {
    vec2 point=gl_FragCoord.xy;
    if(framebuffer_space<0.5) {
        vec3 pixel=vec3(point,1.0);
        point=vec2(dot(physical_x,pixel),dot(physical_y,pixel));
    }
    vec2 local=point-clip_rect.xy;
    vec2 edge=min(local,clip_rect.zw-local);
    float coverage=1.0;
    // Fully covered strips through the rounded box need no distance field.
    // Keep its antialiased straight edges in the SDF path as well as corners,
    // including partially cropped or fractionally transformed client buffers.
    if(min(edge.x,edge.y)<0.5 || (edge.x<radius && edge.y<radius)) {
        vec2 q=vec2(radius)-edge;
        float d=min(max(q.x,q.y),0.0)+length(max(q,0.0))-radius;
        coverage=clamp(0.5-d,0.0,1.0);
    }
    vec4 color=texture2D(tex,v_coords);
#ifdef NO_ALPHA
    color.a=1.0;
#endif
    color*=alpha*coverage;
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

const SOLID_SHADER: &str = r#"
precision highp float;
uniform vec3 physical_x;
uniform vec3 physical_y;
uniform vec4 clip_rect;
uniform float radius;
uniform vec4 fill;
uniform float border;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
float mask(vec2 p,vec2 half_size,float r) {
    vec2 q=abs(p)-half_size+r;
    float d=min(max(q.x,q.y),0.0)+length(max(q,0.0))-r;
    return clamp(0.5-d,0.0,1.0);
}
void main() {
    vec3 pixel=vec3(gl_FragCoord.xy,1.0);
    vec2 p=vec2(dot(physical_x,pixel),dot(physical_y,pixel))-clip_rect.xy-clip_rect.zw*0.5;
    float coverage=mask(p,clip_rect.zw*0.5,radius);
    if(border>0.0) coverage=max(0.0,coverage-mask(p,clip_rect.zw*0.5-border,max(0.0,radius-border)));
    vec4 color=fill*coverage;
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lower_border_trim_preserves_custom_solids_and_degenerate_geometry() {
        let shape = DecorationShape {
            rect: Rect::new(Point::new(10, 20), wm_theme_api::Size::new(100, 80)),
            radius: 9,
            border: 1,
            border_rgb: [120, 80, 40],
        };
        let color = Color32F::new(120.0 / 255.0, 80.0 / 255.0, 40.0 / 255.0, 1.0);
        let bottom = PixelRect::new((10, 99).into(), (100, 1).into());
        let left = PixelRect::new((10, 50).into(), (1, 49).into());
        let right = PixelRect::new((109, 50).into(), (1, 49).into());
        assert_eq!(
            trim_lower_border_strip(bottom, color, shape),
            Some(PixelRect::new((19, 99).into(), (82, 1).into()))
        );
        assert_eq!(
            trim_lower_border_strip(left, color, shape),
            Some(PixelRect::new((10, 50).into(), (1, 41).into()))
        );
        assert_eq!(
            trim_lower_border_strip(right, color, shape),
            Some(PixelRect::new((109, 50).into(), (1, 41).into()))
        );
        assert!(
            trim_lower_border_strip(bottom, Color32F::new(1.0, 1.0, 1.0, 1.0), shape).is_none()
        );
        assert!(trim_lower_border_strip(pixel_rect(shape.rect), color, shape).is_none());
        assert!(trim_lower_border_strip(
            PixelRect::new((10, 20).into(), (100, 1).into()),
            color,
            shape
        )
        .is_none());
        assert!(
            trim_lower_border_strip(bottom, color, DecorationShape { radius: 0, ..shape })
                .is_none()
        );
        assert!(
            trim_lower_border_strip(bottom, color, DecorationShape { border: 0, ..shape })
                .is_none()
        );
        let tiny = DecorationShape {
            rect: Rect::new(Point::new(10, 20), wm_theme_api::Size::new(2, 2)),
            radius: 1,
            ..shape
        };
        assert!(trim_lower_border_strip(
            PixelRect::new((10, 21).into(), (2, 1).into()),
            color,
            tiny
        )
        .unwrap()
        .is_empty());
    }

    use smithay::backend::renderer::{
        damage::OutputDamageTracker,
        element::{
            texture::TextureRenderElement,
            utils::{CropRenderElement, Relocate, RelocateRenderElement},
        },
        Bind, ExportMem, Frame, ImportMem, Offscreen, Renderer,
    };
    use smithay::backend::{
        allocator::Fourcc,
        egl::{native::EGLSurfacelessDisplay, EGLContext, EGLDisplay},
    };
    use wm_theme_api::Size;

    #[test]
    fn framebuffer_rect_preserves_all_signed_axis_permutations_and_rejects_other_maps() {
        let physical = [20.0, 15.0, 70.0, 50.0];
        for swapped in [false, true] {
            for x_sign in [-1.0, 1.0] {
                for y_sign in [-1.0, 1.0] {
                    let affine = if swapped {
                        [[0.0, x_sign, 41.0], [y_sign, 0.0, 53.0]]
                    } else {
                        [[x_sign, 0.0, 41.0], [0.0, y_sign, 53.0]]
                    };
                    let [x, y, w, h] = framebuffer_rect(affine, physical).unwrap();
                    let corners = [(x, y), (x + w, y), (x, y + h), (x + w, y + h)].map(|(x, y)| {
                        [
                            affine[0][0] * x + affine[0][1] * y + affine[0][2],
                            affine[1][0] * x + affine[1][1] * y + affine[1][2],
                        ]
                    });
                    let min = [0, 1]
                        .map(|axis| corners.iter().map(|p| p[axis]).reduce(f32::min).unwrap());
                    let max = [0, 1]
                        .map(|axis| corners.iter().map(|p| p[axis]).reduce(f32::max).unwrap());
                    assert_eq!([min[0], min[1], max[0] - min[0], max[1] - min[1]], physical);
                }
            }
        }
        for affine in [
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            [[1.0, 0.0000001, 0.0], [0.0, 1.0, 0.0]],
            [[0.8, -0.6, 0.0], [0.6, 0.8, 0.0]],
            [[0.0; 3]; 2],
            [[1.0, 0.0, f32::NAN], [0.0, 1.0, 0.0]],
        ] {
            assert!(framebuffer_rect(affine, physical).is_none());
        }
        assert!(
            framebuffer_rect([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], [0.0, 0.0, -1.0, 1.0]).is_none()
        );
    }

    #[test]
    fn opaque_fill_proof_requires_one_current_full_opaque_rectangle() {
        use smithay::backend::renderer::element::solid::SolidColorRenderElement;
        let requested = Rect::new(Point::new(20, 15), Size::new(70, 50));
        let make = |geometry, alpha| {
            SolidColorRenderElement::new(
                Id::new(),
                geometry,
                CommitCounter::default(),
                Color32F::new(0.0, 0.0, 0.0, alpha),
                Kind::Unspecified,
            )
        };
        let full = make(pixel_rect(requested), 1.0);
        assert!(opaque_element_covers(&full, 1, requested));
        assert!(!opaque_element_covers(&full, 0, requested));
        assert!(!opaque_element_covers(&full, 2, requested));
        assert!(!opaque_element_covers(&full, 17, requested));
        assert!(!opaque_element_covers(
            &make(pixel_rect(requested), 0.5),
            1,
            requested
        ));
        assert!(!opaque_element_covers(
            &make(PixelRect::new((20, 15).into(), (69, 50).into()), 1.0),
            1,
            requested
        ));
        assert!(!opaque_element_covers(
            &make(PixelRect::new((21, 15).into(), (70, 50).into()), 1.0),
            1,
            requested
        ));
        assert!(!opaque_element_covers(
            &full,
            1,
            Rect::new(requested.pos, Size::default())
        ));
        let distant = make(
            PixelRect::new((i32::MAX - 2, i32::MAX - 2).into(), (10, 10).into()),
            1.0,
        );
        assert!(opaque_element_covers(
            &distant,
            1,
            Rect::new(Point::new(i32::MAX, i32::MAX), Size::new(4, 4))
        ));
        struct NoOpacityRead;
        impl Element for NoOpacityRead {
            fn id(&self) -> &Id {
                unreachable!()
            }
            fn current_commit(&self) -> CommitCounter {
                unreachable!()
            }
            fn src(&self) -> Rectangle<f64, Buffer> {
                unreachable!()
            }
            fn geometry(&self, _: Scale<f64>) -> PixelRect {
                unreachable!()
            }
            fn opaque_regions(&self, _: Scale<f64>) -> OpaqueRegions<i32, Physical> {
                panic!("fragmented opacity must never be copied")
            }
        }
        for count in [0, 2, 17, usize::MAX] {
            assert!(!opaque_element_covers(&NoOpacityRead, count, requested));
        }
        let (_, allocations) = chonk_test_support::measure(|| {
            for _ in 0..100 {
                assert!(opaque_element_covers(&full, 1, requested));
                assert!(!opaque_element_covers(&full, 17, requested));
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
        // Nonclient planes cannot prove coverage, even if fully opaque.
        assert!(!opaque_client_covers(&[full.into()], requested));
        assert!(!opaque_client_covers(&[], requested));
    }

    #[test]
    #[ignore = "requires a GLES/EGL renderer; run explicitly in native visual gates"]
    fn native_gles_opaque_client_avoids_double_rounded_fill_coverage() {
        use smithay::backend::renderer::element::solid::SolidColorRenderElement;
        // SAFETY: Only Smithay creates and terminates this test's surfaceless EGL display.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        let programs = programs(&mut renderer).unwrap();
        let shape = DecorationShape {
            rect: Rect::new(Point::new(20, 15), Size::new(70, 50)),
            radius: 9,
            border: 0,
            border_rgb: [0; 3],
        };
        let color = [64u8, 128, 192, 255];
        let pixels = color.repeat(70 * 50);
        let client = renderer
            .import_memory(&pixels, Fourcc::Abgr8888, (70, 50).into(), false)
            .unwrap();
        for background in [
            Color32F::TRANSPARENT,
            Color32F::new(0.125, 0.25, 0.375, 1.0),
        ] {
            for (name, width, alpha, region_width, expected_coverage) in [
                ("opaque", 70, 1.0, 70, true),
                ("resize_gap", 35, 1.0, 70, false),
                ("translucent", 70, 0.5, 70, false),
                ("partial_opacity", 70, 1.0, 35, false),
            ] {
                let inner = TextureRenderElement::from_static_texture(
                    Id::new(),
                    renderer.context_id(),
                    (20.0, 15.0),
                    client.clone(),
                    1,
                    Transform::Normal,
                    Some(alpha),
                    Some(Rectangle::from_size((70.0, 50.0).into())),
                    Some((width, 50).into()),
                    Some(vec![Rectangle::from_size((region_width, 50).into())]),
                    Kind::Unspecified,
                );
                let covered = opaque_element_covers(&inner, 1, shape.rect);
                assert_eq!(covered, expected_coverage, "{name}");
                let client_geometry = inner.geometry(1.0.into());
                let element = Rounded {
                    inner,
                    programs: programs.clone(),
                    rect: pixel_rect(shape.rect),
                    radius: 9.0,
                    color: None,
                    texture: Some(client.clone()),
                    border: 0.0,
                };
                let fill = Rounded {
                    inner: SolidColorRenderElement::new(
                        Id::new(),
                        pixel_rect(shape.rect),
                        CommitCounter::default(),
                        Color32F::new(0.0, 0.0, 0.0, 1.0),
                        Kind::Unspecified,
                    ),
                    programs: programs.clone(),
                    rect: pixel_rect(shape.rect),
                    radius: 9.0,
                    color: Some(Color32F::new(0.0, 0.0, 0.0, 1.0)),
                    texture: None,
                    border: 0.0,
                };
                let mut output: GlesTexture = renderer
                    .create_buffer(Fourcc::Abgr8888, (128, 96).into())
                    .unwrap();
                let mut target = renderer.bind(&mut output).unwrap();
                let mut frame = renderer
                    .render(&mut target, (128, 96).into(), Transform::Normal)
                    .unwrap();
                frame
                    .clear(background, &[PixelRect::from_size((128, 96).into())])
                    .unwrap();
                if !covered {
                    fill.draw(
                        &mut frame,
                        fill.src(),
                        fill.geometry(1.0.into()),
                        &[PixelRect::from_size(fill.geometry(1.0.into()).size)],
                        &[],
                    )
                    .unwrap();
                }
                element
                    .draw(
                        &mut frame,
                        element.src(),
                        client_geometry,
                        &[PixelRect::from_size(client_geometry.size)],
                        &element.opaque_regions(1.0.into()),
                    )
                    .unwrap();
                frame.finish().unwrap().wait().unwrap();
                let mapping = renderer
                    .copy_framebuffer(
                        &target,
                        Rectangle::from_size((128, 96).into()),
                        Fourcc::Abgr8888,
                    )
                    .unwrap();
                let actual = renderer.map_texture(&mapping).unwrap();
                for y in 0..96 {
                    for x in 0..128 {
                        let at = Point::new(x, y);
                        let coverage = shape.coverage(at);
                        let client_alpha = if client_geometry.contains((x, y)) {
                            coverage * alpha
                        } else {
                            0.0
                        };
                        let fill_alpha = if covered { 0.0 } else { coverage };
                        let expected_alpha = client_alpha + fill_alpha * (1.0 - client_alpha);
                        for channel in 0..4 {
                            let background_color = [
                                background.r(),
                                background.g(),
                                background.b(),
                                background.a(),
                            ];
                            let value = if channel == 3 {
                                (expected_alpha + background.a() * (1.0 - expected_alpha)) * 255.0
                            } else {
                                f32::from(color[channel]) * client_alpha
                                    + background_color[channel] * (1.0 - expected_alpha) * 255.0
                            };
                            let expected = value.round() as u8;
                            let value = actual[((y * 128 + x) * 4) as usize + channel];
                            assert!(
                                (i16::from(value) - i16::from(expected)).abs() <= 2,
                                "{name} at={x},{y} channel={channel}: {value} expected {expected}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires a GLES/EGL software renderer; run explicitly in native visual gates"]
    fn native_gles_rounded_texture_crop_relocation_and_shadow_pixels() {
        // SAFETY: Only Smithay creates and terminates this test's surfaceless EGL display.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        renderer
            .with_context(|gl| {
                // SAFETY: The current GLES context supplies a NUL-terminated RENDERER
                // string whose storage remains valid throughout this callback.
                let name = unsafe {
                    std::ffi::CStr::from_ptr(
                        gl.GetString(smithay::backend::renderer::gles::ffi::RENDERER)
                            .cast(),
                    )
                };
                eprintln!("native rounded GL renderer: {}", name.to_string_lossy());
            })
            .unwrap();
        let programs = programs(&mut renderer).expect("both rounded shader variants compile");
        let shape = DecorationShape {
            rect: Rect::new(Point::new(20, 15), Size::new(70, 50)),
            radius: 9,
            border: 1,
            border_rgb: [255; 3],
        };
        // This mutable uploaded texture follows the live-client read/write
        // synchronization path. Every case updates it, then samples its clone
        // through a body/corner batch with declared opaque client metadata.
        let mut client_pixels = vec![255; 70 * 50 * 4];
        let client_texture = renderer
            .import_memory(&client_pixels, Fourcc::Abgr8888, (70, 50).into(), false)
            .unwrap();
        let client_opaque = vec![Rectangle::from_size((70, 50).into())];
        let mut generation = 0u8;
        for stretched in [false, true] {
            for crop in [false, true] {
                for moved in [false, true] {
                    for transform in [
                        Transform::Normal,
                        Transform::_90,
                        Transform::_180,
                        Transform::_270,
                        Transform::Flipped,
                        Transform::Flipped90,
                        Transform::Flipped180,
                        Transform::Flipped270,
                    ] {
                        let mut shape = shape;
                        if stretched {
                            shape.rect.size = Size::new(84, 40);
                            shape.radius = 7;
                        }
                        generation = generation.wrapping_add(1);
                        let color = [generation, 160, 224, 255];
                        for pixel in client_pixels.as_chunks_mut::<4>().0 {
                            pixel.copy_from_slice(&color);
                        }
                        renderer
                            .update_memory(
                                &client_texture,
                                &client_pixels,
                                Rectangle::from_size((70, 50).into()),
                            )
                            .unwrap();
                        let inner = TextureRenderElement::from_static_texture(
                            Id::new(),
                            renderer.context_id(),
                            (20.0, 15.0),
                            client_texture.clone(),
                            1,
                            Transform::Normal,
                            None,
                            Some(Rectangle::from_size((70.0, 50.0).into())),
                            Some((shape.rect.size.w as i32, shape.rect.size.h as i32).into()),
                            Some(client_opaque.clone()),
                            Kind::Unspecified,
                        );
                        let element = Rounded {
                            inner,
                            programs: programs.clone(),
                            rect: pixel_rect(shape.rect),
                            radius: f32::from(shape.radius),
                            color: None,
                            texture: Some(client_texture.clone()),
                            border: 0.0,
                        };
                        let crop_rect = if crop {
                            PixelRect::new((24, 18).into(), (65, 45).into())
                        } else {
                            PixelRect::new((0, 0).into(), (128, 96).into())
                        };
                        let cropped =
                            CropRenderElement::from_element(element, 1.0, crop_rect).unwrap();
                        let shift = if moved { (7, 5) } else { (0, 0) };
                        let element =
                            RelocateRenderElement::from_element(cropped, shift, Relocate::Relative);
                        let target_size =
                            transform.transform_size(smithay::utils::Size::<i32, Physical>::from(
                                (128, 96),
                            ));
                        let mut texture: GlesTexture = renderer
                            .create_buffer(Fourcc::Abgr8888, (target_size.w, target_size.h).into())
                            .unwrap();
                        let mut target = renderer.bind(&mut texture).unwrap();
                        let mut damage = OutputDamageTracker::new(target_size, 1.0, transform);
                        damage
                            .render_output(
                                &mut renderer,
                                &mut target,
                                0,
                                &[element],
                                Color32F::TRANSPARENT,
                            )
                            .unwrap();
                        let mapping = renderer
                            .copy_framebuffer(
                                &target,
                                Rectangle::from_size((target_size.w, target_size.h).into()),
                                Fourcc::Abgr8888,
                            )
                            .unwrap();
                        let pixels = renderer.map_texture(&mapping).unwrap().to_vec();
                        drop(mapping);
                        drop(target);
                        for y in 0..target_size.h {
                            for x in 0..target_size.w {
                                let scene = transform.transform_point_in(
                                    smithay::utils::Point::<f64, Physical>::from((
                                        f64::from(x) + 0.5,
                                        f64::from(y) + 0.5,
                                    )),
                                    &target_size.to_f64(),
                                );
                                let at = Point::new(
                                    scene.x.floor() as i32 - shift.0,
                                    scene.y.floor() as i32 - shift.1,
                                );
                                let coverage = if crop_rect.contains((at.x, at.y)) {
                                    shape.coverage(at)
                                } else {
                                    0.0
                                };
                                for (channel, value) in color.iter().enumerate() {
                                    let expected = (coverage * f32::from(*value)).round() as u8;
                                    let actual =
                                        pixels[((y * target_size.w + x) * 4) as usize + channel];
                                    assert!(
                                        (i16::from(actual) - i16::from(expected)).abs() <= 1,
                                        "stretch={stretched} crop={crop} moved={moved} transform={transform:?} at={x},{y} channel={channel}: {actual} expected {expected}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        // Exercise the actual warm draw path, including reusable custom
        // uniforms and Smithay's damage scratch storage, on one live texture.
        let inner = TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            (20.0, 30.0),
            client_texture.clone(),
            1,
            Transform::Normal,
            None,
            Some(Rectangle::from_size((70.0, 50.0).into())),
            Some((70, 35).into()),
            Some(client_opaque),
            Kind::Unspecified,
        );
        let element = Rounded {
            inner,
            programs: programs.clone(),
            rect: pixel_rect(shape.rect),
            radius: f32::from(shape.radius),
            color: None,
            texture: Some(client_texture.clone()),
            border: 0.0,
        };
        let mut texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (128, 96).into())
            .unwrap();
        let mut target = renderer.bind(&mut texture).unwrap();
        let mut frame = renderer
            .render(&mut target, (128, 96).into(), Transform::Normal)
            .unwrap();
        let destination = element.geometry(1.0.into());
        let damage = [PixelRect::from_size(destination.size)];
        let opaque = element.opaque_regions(1.0.into());
        assert_eq!(
            opaque.len(),
            2,
            "a client below the title needs only middle and bottom opacity rectangles"
        );
        let fragmented: [PixelRect; 10] = std::array::from_fn(|index| {
            Rectangle::new((index as i32 * 7, 0).into(), (7, destination.size.h).into())
        });
        for damage in [damage.as_slice(), fragmented.as_slice()] {
            for _ in 0..2 {
                element
                    .draw(&mut frame, element.src(), destination, damage, &opaque)
                    .unwrap();
            }
            let (_, allocations) = chonk_test_support::measure(|| {
                for _ in 0..100 {
                    element
                        .draw(&mut frame, element.src(), destination, damage, &opaque)
                        .unwrap();
                }
            });
            assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
        }
        frame.finish().unwrap().wait().unwrap();
        drop(target);
        let shadow = wm_theme_api::DecorationShadow {
            rect: shape.rect,
            radius: 0,
            offset: Point::new(5, 5),
            blur: 0,
            rgba: [0x77, 0x7d, 0x6b, 0x38],
        };
        let surface = wm_theme_api::DecorationSurface {
            frame_size: Size::new(128, 96),
            parts: Vec::new(),
            solids: Vec::new(),
            shadow: Some(shadow),
            shape: None,
        };
        let mut effects = None;
        crate::frame_effects::FrameEffects::update(&mut effects, &surface);
        let mut scene = Vec::with_capacity(8);
        crate::frame_effects::push_shadow(
            &mut scene,
            &mut renderer,
            effects.as_ref(),
            Point::new(0, 0),
            Point::new(0, 0),
            Point::new(0, 0),
            1.0,
            1.0,
            1.0,
        );
        let mut texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (128, 96).into())
            .unwrap();
        let mut target = renderer.bind(&mut texture).unwrap();
        let mut damage = OutputDamageTracker::new((128, 96), 1.0, Transform::Normal);
        damage
            .render_output(&mut renderer, &mut target, 0, &scene, Color32F::TRANSPARENT)
            .unwrap();
        let mapping = renderer
            .copy_framebuffer(
                &target,
                Rectangle::from_size((128, 96).into()),
                Fourcc::Abgr8888,
            )
            .unwrap();
        let pixels = renderer.map_texture(&mapping).unwrap();
        let shifted = Rect::new(Point::new(25, 20), shape.rect.size);
        for y in 0..96 {
            for x in 0..128 {
                let at = Point::new(x, y);
                let expected = if shifted.contains(at) && !shape.rect.contains(at) {
                    0x38
                } else {
                    0
                };
                assert_eq!(
                    pixels[((y * 128 + x) * 4 + 3) as usize],
                    expected,
                    "Washi shadow {x},{y}"
                );
            }
        }
    }
}
