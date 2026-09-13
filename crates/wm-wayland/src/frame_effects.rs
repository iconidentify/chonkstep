//! Retained frame effects. Effect geometry is independent from input geometry,
//! and shader work is restricted to the perimeter rather than window interiors.
use crate::renderer::SceneElement;
use smithay::backend::renderer::{
    element::{Element, Id, Kind, RenderElement},
    gles::{
        GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, Uniform, UniformName, UniformType,
    },
    utils::CommitCounter,
};
#[cfg(test)]
use smithay::utils::Transform;
use smithay::utils::{Buffer, Physical, Rectangle, Scale};
use wm_theme_api::{DecorationShadow, DecorationShape, DecorationSurface, Point};

#[cfg(test)]
use smithay::backend::renderer::gles::{GlesTexProgram, GlesTexture};

type PixelRect = Rectangle<i32, Physical>;

#[derive(Debug)]
pub(crate) struct FrameEffects {
    pub shadow: Option<DecorationShadow>,
    pub shape: Option<DecorationShape>,
    ids: [Id; 8],
    pub commit: CommitCounter,
    pub border_ids: [Id; 2],
}

impl FrameEffects {
    pub fn update(retained: &mut Option<Self>, surface: &DecorationSurface) {
        let shadow = surface.shadow.map(DecorationShadow::normalized);
        let shape = surface.shape.map(DecorationShape::normalized);
        if shadow.is_none() && shape.is_none() {
            *retained = None;
            return;
        }
        if let Some(old) = retained {
            if old.shadow != shadow || old.shape != shape {
                old.shadow = shadow;
                old.shape = shape;
                old.commit.increment();
            }
        } else {
            *retained = Some(Self {
                shadow,
                shape,
                ids: std::array::from_fn(|_| Id::new()),
                commit: CommitCounter::default(),
                border_ids: std::array::from_fn(|_| Id::new()),
            });
        }
    }

    /// The invisible resize ring is still active. Only transparent points
    /// inside the visual rectangle decline input and expose the window below.
    pub fn accepts_input(&self, point: Point) -> bool {
        self.shape
            .is_none_or(|shape| !shape.rect.contains(point) || shape.coverage(point) > 0.0)
    }

    pub fn client_accepts_input(&self, point: Point) -> bool {
        self.shape.is_none_or(|mut shape| {
            let border = u32::from(shape.border)
                .min(shape.rect.size.w / 2)
                .min(shape.rect.size.h / 2);
            shape.rect.pos.x += border as i32;
            shape.rect.pos.y += border as i32;
            shape.rect.size.w -= 2 * border;
            shape.rect.size.h -= 2 * border;
            shape.radius = shape.radius.saturating_sub(border as u16);
            shape.coverage(point) > 0.0
        })
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ShadowElement {
    id: Id,
    commit: CommitCounter,
    shader: Option<GlesPixelProgram>,
    cached: Option<CachedShadow>,
    geometry: PixelRect,
    source: [f32; 4],
    radius: f32,
    blur: f32,
    offset: [f32; 2],
    rgba: [f32; 4],
    alpha: f32,
}

#[derive(Debug, Clone)]
struct CachedShadow {
    atlas: crate::shadow_cache::Atlas,
    shader: LookupPrograms,
    pixels: u32,
    cuts_x: [i32; 4],
    cuts_y: [i32; 4],
    atlas_edges: [f32; 2],
}

impl Element for ShadowElement {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        // Source includes mask geometry, so moving/rescaling the mask under an
        // unchanged piece still invalidates Smithay's retained damage state.
        Rectangle::new(
            (self.source[0] as f64, self.source[1] as f64).into(),
            (self.source[2] as f64, self.source[3] as f64).into(),
        )
    }
    fn geometry(&self, _: Scale<f64>) -> PixelRect {
        self.geometry
    }
    fn alpha(&self) -> f32 {
        self.alpha
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl RenderElement<GlesRenderer> for ShadowElement {
    fn draw(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: PixelRect,
        damage: &[PixelRect],
        _: &[PixelRect],
    ) -> Result<(), GlesError> {
        let [x, y, w, h] = self.source;
        let [r, g, b, a] = self.rgba;
        if let Some(cached) = &self.cached {
            let sx = (src.loc.x - f64::from(x)) / f64::from(w).max(1.0);
            let sy = (src.loc.y - f64::from(y)) / f64::from(h).max(1.0);
            let shift_x = dst.loc.x as f64
                - f64::from(self.geometry.loc.x)
                - sx * f64::from(self.geometry.size.w);
            let shift_y = dst.loc.y as f64
                - f64::from(self.geometry.loc.y)
                - sy * f64::from(self.geometry.size.h);
            let shift = (shift_x.round() as i32, shift_y.round() as i32);
            let [[xx, xy, xz], [yx, yy, yz]] = frame.framebuffer_to_physical();
            let physical_rect = [x + shift.0 as f32, y + shift.1 as f32, w, h];
            let framebuffer =
                crate::rounded::framebuffer_rect([[xx, xy, xz], [yx, yy, yz]], physical_rect);
            let shader_rect = framebuffer.unwrap_or(physical_rect);
            let uniforms = [
                Uniform::new("physical_x", (xx, xy, xz)),
                Uniform::new("physical_y", (yx, yy, yz)),
                Uniform::new(
                    "box_rect",
                    (
                        shader_rect[0],
                        shader_rect[1],
                        shader_rect[2],
                        shader_rect[3],
                    ),
                ),
                Uniform::new(
                    "framebuffer_box",
                    if framebuffer.is_some() {
                        1.0_f32
                    } else {
                        0.0_f32
                    },
                ),
                Uniform::new("corner_radius", self.radius),
                Uniform::new("atlas", 0_i32),
            ];
            let visual = PixelRect::new(
                (
                    (x as i32).saturating_add(shift.0),
                    (y as i32).saturating_add(shift.1),
                )
                    .into(),
                (w as i32, h as i32).into(),
            );
            let radius = self.radius as i32;
            let corners = [
                (visual.loc.x, visual.loc.y),
                (visual.loc.x + visual.size.w - radius, visual.loc.y),
                (visual.loc.x, visual.loc.y + visual.size.h - radius),
                (
                    visual.loc.x + visual.size.w - radius,
                    visual.loc.y + visual.size.h - radius,
                ),
            ]
            .map(|p| PixelRect::new(p.into(), (radius, radius).into()));
            let local_x = cached
                .cuts_x
                .map(|cut| (i64::from(cut) - i64::from(self.geometry.loc.x)) as f32);
            let local_y = cached
                .cuts_y
                .map(|cut| (i64::from(cut) - i64::from(self.geometry.loc.y)) as f32);
            let slice_uniforms = [
                Uniform::new("slice_x", (local_x[0], local_x[1], local_x[2], local_x[3])),
                Uniform::new("slice_y", (local_y[0], local_y[1], local_y[2], local_y[3])),
                Uniform::new(
                    "atlas_edges",
                    (cached.atlas_edges[0], cached.atlas_edges[1]),
                ),
                Uniform::new("atlas", 0_i32),
            ];
            let masked_uniforms = [
                uniforms[0].clone(),
                uniforms[1].clone(),
                uniforms[2].clone(),
                uniforms[3].clone(),
                uniforms[4].clone(),
                slice_uniforms[0].clone(),
                slice_uniforms[1].clone(),
                slice_uniforms[2].clone(),
                slice_uniforms[3].clone(),
            ];
            let full = PixelRect::new(
                (
                    self.geometry.loc.x.saturating_add(shift.0),
                    self.geometry.loc.y.saturating_add(shift.1),
                )
                    .into(),
                self.geometry.size,
            );
            // Every instance is clipped to one affine atlas tile. Its midpoint
            // selects that tile in the vertex shader, including coincident
            // neighboring cuts at small fractional presentation scales.
            return cached.atlas.with_bound(frame, |frame| {
                for chunk in damage.chunks(16) {
                    // 16 damaged rectangles × 8 tiles × at most 4 disjoint
                    // outside strips or corner intersections per tile.
                    let mut outside = [PixelRect::default(); 512];
                    let mut outside_len = 0;
                    let mut curved = [PixelRect::default(); 512];
                    let mut curved_len = 0;
                    for row in 0..3 {
                        for col in 0..3 {
                            if row == 1 && col == 1 {
                                continue;
                            }
                            let tile = PixelRect::new(
                                (
                                    cached.cuts_x[col].saturating_add(shift.0),
                                    cached.cuts_y[row].saturating_add(shift.1),
                                )
                                    .into(),
                                (
                                    cached.cuts_x[col + 1] - cached.cuts_x[col],
                                    cached.cuts_y[row + 1] - cached.cuts_y[row],
                                )
                                    .into(),
                            );
                            if tile.is_empty() {
                                continue;
                            }
                            for part in chunk {
                                let absolute = PixelRect::new(part.loc + dst.loc, part.size);
                                if let Some(part) = absolute.intersection(tile) {
                                    if let Some(hole) = part.intersection(visual) {
                                        let right = part.loc.x + part.size.w;
                                        let bottom = part.loc.y + part.size.h;
                                        let hr = hole.loc.x + hole.size.w;
                                        let hb = hole.loc.y + hole.size.h;
                                        for mut piece in [
                                            PixelRect::new(
                                                part.loc,
                                                (part.size.w, hole.loc.y - part.loc.y).into(),
                                            ),
                                            PixelRect::new(
                                                (part.loc.x, hb).into(),
                                                (part.size.w, bottom - hb).into(),
                                            ),
                                            PixelRect::new(
                                                (part.loc.x, hole.loc.y).into(),
                                                (hole.loc.x - part.loc.x, hole.size.h).into(),
                                            ),
                                            PixelRect::new(
                                                (hr, hole.loc.y).into(),
                                                (right - hr, hole.size.h).into(),
                                            ),
                                        ] {
                                            if !piece.is_empty() {
                                                piece.loc -= full.loc;
                                                outside[outside_len] = piece;
                                                outside_len += 1;
                                            }
                                        }
                                        for corner in corners {
                                            if let Some(mut piece) = hole.intersection(corner) {
                                                piece.loc -= full.loc;
                                                curved[curved_len] = piece;
                                                curved_len += 1;
                                            }
                                        }
                                    } else {
                                        let mut part = part;
                                        part.loc -= full.loc;
                                        outside[outside_len] = part;
                                        outside_len += 1;
                                    }
                                }
                            }
                        }
                    }
                    let source = Rectangle::from_size(
                        (f64::from(cached.pixels), f64::from(cached.pixels)).into(),
                    );
                    let size = (cached.pixels as i32, cached.pixels as i32).into();
                    if outside_len > 0 {
                        frame.render_pixel_shader_to(
                            &cached.shader.plain,
                            source,
                            full,
                            size,
                            Some(&outside[..outside_len]),
                            self.alpha,
                            &slice_uniforms,
                        )?;
                    }
                    if curved_len > 0 {
                        frame.render_pixel_shader_to(
                            &cached.shader.masked,
                            source,
                            full,
                            size,
                            Some(&curved[..curved_len]),
                            self.alpha,
                            &masked_uniforms,
                        )?;
                    }
                }
                Ok(())
            });
        }
        let uniforms = [
            Uniform::new("box_rect", (x, y, w, h)),
            Uniform::new("corner_radius", self.radius),
            Uniform::new("blur_sigma", self.blur * 0.5),
            Uniform::new("shadow_offset", (self.offset[0], self.offset[1])),
            Uniform::new("shadow_color", (r, g, b, a)),
            Uniform::new(
                "piece_origin",
                (self.geometry.loc.x as f32, self.geometry.loc.y as f32),
            ),
        ];
        // A crop changes both src and dst. Derive its position relative to the
        // retained piece explicitly; the virtual source above is damage state.
        let sx = f64::from(self.geometry.size.w) / f64::from(w).max(1.0);
        let sy = f64::from(self.geometry.size.h) / f64::from(h).max(1.0);
        let source = Rectangle::new(
            (
                (src.loc.x - f64::from(x)) * sx,
                (src.loc.y - f64::from(y)) * sy,
            )
                .into(),
            (src.size.w * sx, src.size.h * sy).into(),
        );
        frame.render_pixel_shader_to(
            self.shader.as_ref().expect("analytic shadow shader"),
            source,
            dst,
            (self.geometry.size.w, self.geometry.size.h).into(),
            Some(damage),
            self.alpha,
            &uniforms,
        )
    }
}

#[derive(Debug)]
struct ShadowProgram(Option<GlesPixelProgram>);

#[derive(Debug)]
struct LookupProgram(Option<LookupPrograms>);

#[derive(Debug, Clone)]
struct LookupPrograms {
    plain: GlesPixelProgram,
    masked: GlesPixelProgram,
}

fn lookup_program(renderer: &mut GlesRenderer) -> Option<LookupPrograms> {
    if let Some(cached) = renderer.egl_context().user_data().get::<LookupProgram>() {
        return cached.0.clone();
    }
    let names = [
        UniformName::new("physical_x", UniformType::_3f),
        UniformName::new("physical_y", UniformType::_3f),
        UniformName::new("box_rect", UniformType::_4f),
        UniformName::new("corner_radius", UniformType::_1f),
        UniformName::new("atlas", UniformType::_1i),
        UniformName::new("framebuffer_box", UniformType::_1f),
        UniformName::new("slice_x", UniformType::_4f),
        UniformName::new("slice_y", UniformType::_4f),
        UniformName::new("atlas_edges", UniformType::_2f),
    ];
    let program = (|| {
        let masked = renderer.compile_custom_pixel_shader_with_vertex(
            LOOKUP_VERTEX_SHADER,
            LOOKUP_SHADER,
            &names,
        )?;
        let plain = renderer.compile_custom_pixel_shader_with_vertex(
            LOOKUP_VERTEX_SHADER,
            PLAIN_LOOKUP_SHADER,
            &[
                UniformName::new("atlas", UniformType::_1i),
                UniformName::new("slice_x", UniformType::_4f),
                UniformName::new("slice_y", UniformType::_4f),
                UniformName::new("atlas_edges", UniformType::_2f),
            ],
        )?;
        Ok::<_, GlesError>(LookupPrograms { plain, masked })
    })()
    .map_err(|error| tracing::warn!(?error, "shadow lookup shader unavailable"))
    .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| LookupProgram(program.clone()));
    program
}

fn shadow_program(renderer: &mut GlesRenderer) -> Option<GlesPixelProgram> {
    if let Some(cached) = renderer.egl_context().user_data().get::<ShadowProgram>() {
        return cached.0.clone();
    }
    let names = [
        UniformName::new("box_rect", UniformType::_4f),
        UniformName::new("corner_radius", UniformType::_1f),
        UniformName::new("blur_sigma", UniformType::_1f),
        UniformName::new("shadow_offset", UniformType::_2f),
        UniformName::new("shadow_color", UniformType::_4f),
        UniformName::new("piece_origin", UniformType::_2f),
    ];
    let program = renderer
        .compile_custom_pixel_shader(SHADOW_SHADER, &names)
        .map_err(|error| tracing::warn!(?error, "frame shadow shader unavailable"))
        .ok();
    renderer
        .egl_context()
        .user_data()
        .insert_if_missing(|| ShadowProgram(program.clone()));
    program
}

/// Source geometry and all piece endpoints use the same transform, avoiding
/// seams at fractional overview/Flow zoom. No scene or uniform Vec is allocated.
#[allow(clippy::too_many_arguments)]
pub(crate) fn push_shadow(
    elements: &mut Vec<SceneElement>,
    renderer: &mut GlesRenderer,
    effects: Option<&FrameEffects>,
    frame: Point,
    source: Point,
    destination: Point,
    sx: f64,
    sy: f64,
    alpha: f32,
) {
    if !sx.is_finite() || !sy.is_finite() || sx <= 0.0 || sy <= 0.0 {
        return;
    }
    let Some(effects) = effects else {
        return;
    };
    let Some(shadow) = effects.shadow.filter(|s| s.rgba[3] > 0) else {
        return;
    };
    // Nonuniform presentation retains the analytic minimum-axis blur/radius
    // contract. Ordinary live resize changes rect, not this presentation scale.
    let atlas = ((sx - sy).abs() < 1.0e-9)
        .then(|| crate::shadow_cache::atlas(renderer, shadow))
        .flatten()
        .and_then(|atlas| lookup_program(renderer).map(|program| (atlas, program)));
    let program = if atlas.is_none() {
        shadow_program(renderer)
    } else {
        None
    };
    if atlas.is_none() && program.is_none() {
        return;
    }
    let transform =
        |rect| crate::overview::scaled_chrome_rect(rect, frame, source, destination, sx, sy);
    let bounds = transform(shadow.effect_bounds());
    let rect = transform(shadow.rect);
    if bounds.size.w == 0 || bounds.size.h == 0 || rect.size.w == 0 || rect.size.h == 0 {
        return;
    }
    let radius = f64::from(shadow.radius) * sx.min(sy);
    let radius = (radius.round().max(0.0) as u32)
        .min(rect.size.w / 2)
        .min(rect.size.h / 2) as i32;
    let right = bounds.pos.x.saturating_add_unsigned(bounds.size.w);
    let bottom = bounds.pos.y.saturating_add_unsigned(bounds.size.h);
    let mut xs = [
        bounds.pos.x,
        rect.pos.x.saturating_add(radius).clamp(bounds.pos.x, right),
        rect.pos
            .x
            .saturating_add_unsigned(rect.size.w)
            .saturating_sub(radius)
            .clamp(bounds.pos.x, right),
        right,
    ];
    let mut ys = [
        bounds.pos.y,
        rect.pos
            .y
            .saturating_add(radius)
            .clamp(bounds.pos.y, bottom),
        rect.pos
            .y
            .saturating_add_unsigned(rect.size.h)
            .saturating_sub(radius)
            .clamp(bounds.pos.y, bottom),
        bottom,
    ];
    if let Some((atlas, _)) = &atlas {
        let inner = wm_theme_api::Rect::new(
            Point::new(
                shadow.rect.pos.x + shadow.offset.x + atlas.inset as i32,
                shadow.rect.pos.y + shadow.offset.y + atlas.inset as i32,
            ),
            wm_theme_api::Size::new(
                shadow.rect.size.w - 2 * atlas.inset,
                shadow.rect.size.h - 2 * atlas.inset,
            ),
        );
        let inner = transform(inner);
        xs[1] = inner.pos.x.clamp(bounds.pos.x, right);
        xs[2] = inner
            .pos
            .x
            .saturating_add_unsigned(inner.size.w)
            .clamp(xs[1], right);
        ys[1] = inner.pos.y.clamp(bounds.pos.y, bottom);
        ys[2] = inner
            .pos
            .y
            .saturating_add_unsigned(inner.size.h)
            .clamp(ys[1], bottom);
    }
    if let Some((atlas, program)) = atlas {
        elements.push(
            ShadowElement {
                id: effects.ids[0].clone(),
                commit: effects.commit,
                shader: None,
                cached: Some(CachedShadow {
                    atlas: atlas.clone(),
                    shader: program,
                    pixels: atlas.pixels,
                    cuts_x: xs,
                    cuts_y: ys,
                    atlas_edges: [
                        (atlas.inset + atlas.pad) as f32 / atlas.side as f32,
                        (atlas.inset + atlas.pad + 1) as f32 / atlas.side as f32,
                    ],
                }),
                geometry: Rectangle::new(
                    (bounds.pos.x, bounds.pos.y).into(),
                    (bounds.size.w as i32, bounds.size.h as i32).into(),
                ),
                source: [
                    rect.pos.x as f32,
                    rect.pos.y as f32,
                    rect.size.w as f32,
                    rect.size.h as f32,
                ],
                radius: radius as f32,
                blur: (f64::from(shadow.blur) * sx.min(sy)) as f32,
                offset: [
                    (f64::from(shadow.offset.x) * sx) as f32,
                    (f64::from(shadow.offset.y) * sy) as f32,
                ],
                rgba: shadow.rgba.map(|c| f32::from(c) / 255.0),
                alpha,
            }
            .into(),
        );
        return;
    }
    let mut index = 0;
    for row in 0..3 {
        for col in 0..3 {
            if row == 1 && col == 1 {
                continue;
            }
            let id = &effects.ids[index];
            index += 1;
            if xs[col + 1] <= xs[col] || ys[row + 1] <= ys[row] {
                continue;
            }
            elements.push(
                ShadowElement {
                    id: id.clone(),
                    commit: effects.commit,
                    shader: program.clone(),
                    cached: None,
                    geometry: Rectangle::new(
                        (xs[col], ys[row]).into(),
                        (xs[col + 1] - xs[col], ys[row + 1] - ys[row]).into(),
                    ),
                    source: [
                        rect.pos.x as f32,
                        rect.pos.y as f32,
                        rect.size.w as f32,
                        rect.size.h as f32,
                    ],
                    radius: radius as f32,
                    blur: (f64::from(shadow.blur) * sx.min(sy)) as f32,
                    offset: [
                        (f64::from(shadow.offset.x) * sx) as f32,
                        (f64::from(shadow.offset.y) * sy) as f32,
                    ],
                    rgba: shadow.rgba.map(|c| f32::from(c) / 255.0),
                    alpha,
                }
                .into(),
            );
        }
    }
}

// Atlas mapping is affine inside each damage instance. Selecting by its
// midpoint distinguishes both sides of a collapsed middle slice; selecting
// separately at its vertices would map their shared endpoint inconsistently.
const LOOKUP_VERTEX_SHADER: &str = r#"#version 100
uniform mat3 matrix;
attribute vec2 vert;
attribute vec4 vert_position;
uniform vec4 slice_x;
uniform vec4 slice_y;
uniform vec2 atlas_edges;
varying vec2 v_coords;
float atlas_axis(float value, float midpoint, vec4 cuts) {
    if (midpoint < cuts.y) {
        return (value-cuts.x) / max(cuts.y-cuts.x, 1.0) * atlas_edges.x;
    }
    if (midpoint >= cuts.z) {
        return atlas_edges.y + (value-cuts.z) / max(cuts.w-cuts.z, 1.0) * (1.0-atlas_edges.y);
    }
    return atlas_edges.x + (value-cuts.y) / max(cuts.z-cuts.y, 1.0) * (atlas_edges.y-atlas_edges.x);
}
void main() {
    vec2 position=vert*vert_position.zw+vert_position.xy;
    vec2 midpoint=vert_position.xy+vert_position.zw*0.5;
    v_coords=vec2(atlas_axis(position.x,midpoint.x,slice_x),atlas_axis(position.y,midpoint.y,slice_y));
    gl_Position=vec4(matrix*vec3(position,1.0),1.0);
}
"#;

// Lookup alpha contains only the Gaussian; the silhouette is evaluated at the
// destination so atlas filtering never softens the frame's one-pixel outline.
const LOOKUP_SHADER: &str = r#"
precision highp float;
uniform sampler2D atlas;
uniform float alpha;
varying vec2 v_coords;
uniform vec3 physical_x;
uniform vec3 physical_y;
uniform vec4 box_rect;
uniform float framebuffer_box;
uniform float corner_radius;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
void main() {
    vec3 pixel=vec3(gl_FragCoord.xy,1.0);
    vec2 position=framebuffer_box>0.5 ? gl_FragCoord.xy : vec2(dot(physical_x,pixel),dot(physical_y,pixel));
    vec2 edge=min(position-box_rect.xy,box_rect.xy+box_rect.zw-position);
    float visible=1.0;
    if(min(edge.x,edge.y)>-0.5) {
        vec2 q=abs(position-box_rect.xy-box_rect.zw*0.5)-box_rect.zw*0.5+corner_radius;
        float distance=min(max(q.x,q.y),0.0)+length(max(q,0.0))-corner_radius;
        visible=1.0-clamp(0.5-distance,0.0,1.0);
    }
    vec4 color=texture2D(atlas,v_coords)*alpha*visible;
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

const PLAIN_LOOKUP_SHADER: &str = r#"
precision highp float;
varying vec2 v_coords;
uniform sampler2D atlas;
uniform float alpha;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
void main() {
    vec4 color=texture2D(atlas,v_coords)*alpha;
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

#[cfg(test)]
const LEGACY_LOOKUP_SHADER: &str = r#"
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
uniform vec4 box_rect;
uniform float corner_radius;
uniform vec4 shadow_color;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
void main() {
    vec3 pixel=vec3(gl_FragCoord.xy,1.0);
    vec2 position=vec2(dot(physical_x,pixel),dot(physical_y,pixel));
    vec2 p=position-box_rect.xy-box_rect.zw*0.5;
    vec2 q=abs(p)-box_rect.zw*0.5+corner_radius;
    float distance=min(max(q.x,q.y),0.0)+length(max(q,0.0))-corner_radius;
    float visible=1.0-clamp(0.5-distance,0.0,1.0);
    float a=texture2D(tex,v_coords).a*shadow_color.a*alpha*visible;
    vec4 color=vec4(shadow_color.rgb*a,a);
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

// Analytic fallback for zero-blur or tiny/displaced frames. The x integral is analytic;
// the y integral uses eight fixed midpoint samples over the nonzero 3-sigma
// support. CSS specifies a Gaussian approximation rather than one mandated
// browser kernel, so this preserves the reference tokens without claiming
// byte-identical browser rasterization. The zero-blur Washi shadow is exact.
const SHADOW_SHADER: &str = r#"
precision highp float;
varying vec2 v_coords;
uniform vec2 size;
uniform float alpha;
uniform vec4 box_rect;
uniform float corner_radius;
uniform float blur_sigma;
uniform vec2 shadow_offset;
uniform vec4 shadow_color;
uniform vec2 piece_origin;
#ifdef DEBUG_FLAGS
uniform float tint;
#endif
float coverage(vec2 p, vec2 half_size, float radius) {
    vec2 q = abs(p) - half_size + radius;
    float distance = min(max(q.x,q.y),0.0) + length(max(q,0.0)) - radius;
    return clamp(0.5-distance,0.0,1.0);
}
vec2 erf_approx(vec2 x) {
    vec2 s = sign(x); x = abs(x);
    vec2 t = 1.0/(1.0+0.3275911*x);
    return s*(1.0-(((((1.061405429*t-1.453152027)*t+1.421413741)*t-0.284496736)*t+0.254829592)*t)*exp(-x*x));
}
float integral(float lo, float hi, float sigma) {
    vec2 e = erf_approx(vec2(lo,hi)/(sigma*1.41421356237));
    return 0.5*(e.y-e.x);
}
void main() {
    vec2 p = piece_origin + v_coords*size - box_rect.xy - box_rect.zw*0.5;
    vec2 half_size = box_rect.zw*0.5;
    float visible = 1.0-coverage(p,half_size,corner_radius);
    p -= shadow_offset;
    float value;
    if (blur_sigma < 0.01) {
        value = coverage(p,half_size,corner_radius);
    } else if (corner_radius < 0.01) {
        value = integral(-half_size.x-p.x,half_size.x-p.x,blur_sigma)*integral(-half_size.y-p.y,half_size.y-p.y,blur_sigma);
    } else {
        float lo = max(-half_size.y,p.y-3.0*blur_sigma);
        float hi = min(half_size.y,p.y+3.0*blur_sigma);
        float step = max(0.0,hi-lo)/8.0;
        value = 0.0;
        for(int i=0;i<8;i++) {
            float y=lo+(float(i)+0.5)*step;
            float dy=max(abs(y)-(half_size.y-corner_radius),0.0);
            float reach=half_size.x-corner_radius+sqrt(max(0.0,corner_radius*corner_radius-dy*dy));
            float n=(y-p.y)/blur_sigma;
            value += integral(-reach-p.x,reach-p.x,blur_sigma)*exp(-0.5*n*n)*step/(blur_sigma*2.50662827463);
        }
    }
    float a=clamp(value,0.0,1.0)*visible*shadow_color.a*alpha;
    vec4 color=vec4(shadow_color.rgb*a,a);
#ifdef DEBUG_FLAGS
    if(tint==1.0) color=vec4(0.0,0.2,0.0,0.2)+color*0.8;
#endif
    gl_FragColor=color;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::{Rect, Size};

    // The pre-batching draw contract is kept as a native oracle: each tile
    // interpolates its own atlas UVs through Smithay's texture matrix. This
    // catches changes hidden by comparing only an ideal continuous Gaussian
    // when fractional destination endpoints are rounded to physical pixels.
    struct LegacyCachedTile {
        atlas: crate::shadow_cache::Atlas,
        element: ShadowElement,
        source: Rectangle<f64, Buffer>,
        shader: GlesTexProgram,
    }
    impl Element for LegacyCachedTile {
        fn id(&self) -> &Id {
            self.element.id()
        }
        fn current_commit(&self) -> CommitCounter {
            self.element.current_commit()
        }
        fn src(&self) -> Rectangle<f64, Buffer> {
            self.element.src()
        }
        fn geometry(&self, scale: Scale<f64>) -> PixelRect {
            self.element.geometry(scale)
        }
        fn alpha(&self) -> f32 {
            self.element.alpha()
        }
    }
    impl RenderElement<GlesRenderer> for LegacyCachedTile {
        fn draw(
            &self,
            frame: &mut GlesFrame<'_, '_>,
            src: Rectangle<f64, Buffer>,
            dst: PixelRect,
            damage: &[PixelRect],
            _: &[PixelRect],
        ) -> Result<(), GlesError> {
            let [x, y, w, h] = self.element.source;
            let [r, g, b, a] = self.element.rgba;
            let sx = (src.loc.x - f64::from(x)) / f64::from(w).max(1.0);
            let sy = (src.loc.y - f64::from(y)) / f64::from(h).max(1.0);
            let source = Rectangle::new(
                (
                    self.source.loc.x + sx * self.source.size.w,
                    self.source.loc.y + sy * self.source.size.h,
                )
                    .into(),
                (
                    self.source.size.w * src.size.w / f64::from(w).max(1.0),
                    self.source.size.h * src.size.h / f64::from(h).max(1.0),
                )
                    .into(),
            );
            let shift_x = dst.loc.x as f32
                - self.element.geometry.loc.x as f32
                - (sx * f64::from(self.element.geometry.size.w)) as f32;
            let shift_y = dst.loc.y as f32
                - self.element.geometry.loc.y as f32
                - (sy * f64::from(self.element.geometry.size.h)) as f32;
            let [[xx, xy, xz], [yx, yy, yz]] = frame.framebuffer_to_physical();
            frame.render_texture_from_to(
                self.atlas.texture(),
                source,
                dst,
                damage,
                &[],
                Transform::Normal,
                self.element.alpha,
                Some(&self.shader),
                &[
                    Uniform::new("physical_x", (xx, xy, xz)),
                    Uniform::new("physical_y", (yx, yy, yz)),
                    Uniform::new("box_rect", (x + shift_x, y + shift_y, w, h)),
                    Uniform::new("corner_radius", self.element.radius),
                    Uniform::new("shadow_color", (r, g, b, a)),
                ],
            )
        }
    }

    #[test]
    #[ignore = "requires GLES/EGL; exercises native cached shadow pixels and transforms"]
    fn native_cached_gaussian_shadow_crop_relocation_and_warm_allocation() {
        use smithay::backend::{
            allocator::Fourcc,
            egl::{native::EGLSurfacelessDisplay, EGLContext, EGLDisplay},
            renderer::{
                damage::OutputDamageTracker,
                element::utils::{CropRenderElement, Relocate, RelocateRenderElement},
                Bind, Color32F, ExportMem, Frame, Offscreen, Renderer,
            },
        };
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
                eprintln!("native shadow GL renderer: {}", name.to_string_lossy());
            })
            .unwrap();
        let shadow = DecorationShadow {
            rect: Rect::new(Point::new(60, 50), Size::new(340, 180)),
            radius: 9,
            offset: Point::new(0, 10),
            blur: 30,
            rgba: [6, 9, 22, 112],
        };
        for (radius, blur) in [(3, 30), (9, 30), (6, 60), (18, 60)] {
            let mut sample = shadow;
            sample.radius = radius;
            sample.blur = blur;
            sample.rect.size = Size::new(800, 600);
            let started = std::time::Instant::now();
            assert!(crate::shadow_cache::atlas(&mut renderer, sample).is_some());
            eprintln!(
                "cold Gaussian atlas radius={radius} blur={blur} CPU+upload: {} us",
                started.elapsed().as_micros()
            );
        }
        let started = std::time::Instant::now();
        assert!(lookup_program(&mut renderer).is_some());
        eprintln!(
            "cold Gaussian lookup shader compilation: {} us",
            started.elapsed().as_micros()
        );
        let legacy_shader = renderer
            .compile_custom_texture_shader(
                LEGACY_LOOKUP_SHADER,
                &[
                    UniformName::new("physical_x", UniformType::_3f),
                    UniformName::new("physical_y", UniformType::_3f),
                    UniformName::new("box_rect", UniformType::_4f),
                    UniformName::new("corner_radius", UniformType::_1f),
                    UniformName::new("shadow_color", UniformType::_4f),
                ],
            )
            .unwrap();
        fn read_pixels<E: RenderElement<GlesRenderer>>(
            renderer: &mut GlesRenderer,
            elements: &[E],
            target_size: smithay::utils::Size<i32, Physical>,
            transform: Transform,
        ) -> Vec<u8> {
            let mut texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (target_size.w, target_size.h).into())
                .unwrap();
            let mut target = renderer.bind(&mut texture).unwrap();
            OutputDamageTracker::new(target_size, 1.0, transform)
                .render_output(renderer, &mut target, 0, elements, Color32F::TRANSPARENT)
                .unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((target_size.w, target_size.h).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            renderer.map_texture(&mapping).unwrap().to_vec()
        }
        let mut white_shadow = shadow;
        white_shadow.rgba = [255; 4];
        let legacy_atlas = crate::shadow_cache::atlas(&mut renderer, white_shadow).unwrap();
        let mut effects = None;
        FrameEffects::update(
            &mut effects,
            &DecorationSurface {
                frame_size: Size::new(480, 320),
                parts: Vec::new(),
                solids: Vec::new(),
                shape: None,
                shadow: Some(shadow),
            },
        );
        let mut scene = Vec::with_capacity(8);
        let push = |scene: &mut Vec<SceneElement>, renderer: &mut GlesRenderer| {
            push_shadow(
                scene,
                renderer,
                effects.as_ref(),
                Point::new(0, 0),
                Point::new(0, 0),
                Point::new(0, 0),
                1.0,
                1.0,
                1.0,
            )
        };
        push(&mut scene, &mut renderer);
        assert_eq!(scene.len(), 1);
        assert!(scene
            .iter()
            .all(|element| matches!(element,SceneElement::Shadow(s) if s.cached.is_some())));
        let (_, allocations) = chonk_test_support::measure(|| {
            for _ in 0..100 {
                scene.clear();
                push(&mut scene, &mut renderer);
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
        // Exercise actual drawing, including the second stack chunk. Scene
        // emission alone does not cover renderer scratch and uniform storage.
        let SceneElement::Shadow(element) = &scene[0] else {
            unreachable!()
        };
        let destination = element.geometry(1.0.into());
        let mut damage = Vec::new();
        for row in 0..8 {
            for col in 0..4 {
                let x0 = destination.size.w * col / 4;
                let x1 = destination.size.w * (col + 1) / 4;
                let y0 = destination.size.h * row / 8;
                let y1 = destination.size.h * (row + 1) / 8;
                damage.push(PixelRect::new((x0, y0).into(), (x1 - x0, y1 - y0).into()));
            }
        }
        let mut texture: GlesTexture = renderer
            .create_buffer(Fourcc::Abgr8888, (480, 320).into())
            .unwrap();
        let mut target = renderer.bind(&mut texture).unwrap();
        let mut frame = renderer
            .render(&mut target, (480, 320).into(), Transform::Normal)
            .unwrap();
        let first_draw = std::time::Instant::now();
        element
            .draw(&mut frame, element.src(), destination, &damage, &[])
            .unwrap();
        frame.finish().unwrap().wait().unwrap();
        eprintln!(
            "first Gaussian shadow draw + completion (driver JIT included): {} us",
            first_draw.elapsed().as_micros()
        );
        let mut frame = renderer
            .render(&mut target, (480, 320).into(), Transform::Normal)
            .unwrap();
        for _ in 0..2 {
            element
                .draw(&mut frame, element.src(), destination, &damage, &[])
                .unwrap();
        }
        let (_, draw_allocations) = chonk_test_support::measure(|| {
            for _ in 0..100 {
                element
                    .draw(&mut frame, element.src(), destination, &damage, &[])
                    .unwrap();
            }
        });
        frame.finish().unwrap().wait().unwrap();
        drop(target);
        assert_eq!(
            (draw_allocations.calls, draw_allocations.requested_bytes),
            (0, 0)
        );
        for authored_size in [Size::new(340, 180), Size::new(109, 109)] {
            let mut shadow = shadow;
            shadow.rect.size = authored_size;
            FrameEffects::update(
                &mut effects,
                &DecorationSurface {
                    frame_size: Size::new(480, 320),
                    parts: Vec::new(),
                    solids: Vec::new(),
                    shape: None,
                    shadow: Some(shadow),
                },
            );
            for presentation_scale in [0.25_f64, 0.5, 1.0, 1.5, 2.0] {
                let scaled_rect = crate::overview::scaled_chrome_rect(
                    shadow.rect,
                    Point::new(0, 0),
                    Point::new(0, 0),
                    Point::new(0, 0),
                    presentation_scale,
                    presentation_scale,
                );
                let scaled_bounds = crate::overview::scaled_chrome_rect(
                    shadow.effect_bounds(),
                    Point::new(0, 0),
                    Point::new(0, 0),
                    Point::new(0, 0),
                    presentation_scale,
                    presentation_scale,
                );
                let scene_size: smithay::utils::Size<i32, Physical> = (
                    (480.0 * presentation_scale).round() as i32,
                    (320.0 * presentation_scale).round() as i32,
                )
                    .into();
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
                    for cropped in [false, true] {
                        scene.clear();
                        push_shadow(
                            &mut scene,
                            &mut renderer,
                            effects.as_ref(),
                            Point::new(0, 0),
                            Point::new(0, 0),
                            Point::new(0, 0),
                            presentation_scale,
                            presentation_scale,
                            1.0,
                        );
                        let clip = if cropped {
                            PixelRect::new(
                                (
                                    (35.0 * presentation_scale).round() as i32,
                                    (35.0 * presentation_scale).round() as i32,
                                )
                                    .into(),
                                (
                                    (380.0 * presentation_scale).round() as i32,
                                    (230.0 * presentation_scale).round() as i32,
                                )
                                    .into(),
                            )
                        } else {
                            PixelRect::from_size(scene_size)
                        };
                        let shift = (7, 5);
                        let SceneElement::Shadow(base) = &scene[0] else {
                            unreachable!()
                        };
                        let cached = base.cached.as_ref().unwrap();
                        let cuts = [
                            0.0,
                            f64::from(cached.atlas_edges[0]) * f64::from(cached.pixels),
                            f64::from(cached.atlas_edges[1]) * f64::from(cached.pixels),
                            f64::from(cached.pixels),
                        ];
                        let mut legacy = Vec::new();
                        let mut id = 0;
                        for row in 0..3 {
                            for col in 0..3 {
                                if row == 1 && col == 1 {
                                    continue;
                                }
                                let mut element = base.clone();
                                element.id = effects.as_ref().unwrap().ids[id].clone();
                                id += 1;
                                element.geometry = PixelRect::new(
                                    (cached.cuts_x[col], cached.cuts_y[row]).into(),
                                    (
                                        cached.cuts_x[col + 1] - cached.cuts_x[col],
                                        cached.cuts_y[row + 1] - cached.cuts_y[row],
                                    )
                                        .into(),
                                );
                                if element.geometry.is_empty() {
                                    continue;
                                }
                                let tile = LegacyCachedTile {
                                    atlas: legacy_atlas.clone(),
                                    element,
                                    source: Rectangle::new(
                                        (cuts[col], cuts[row]).into(),
                                        (cuts[col + 1] - cuts[col], cuts[row + 1] - cuts[row])
                                            .into(),
                                    ),
                                    shader: legacy_shader.clone(),
                                };
                                if let Some(element) =
                                    CropRenderElement::from_element(tile, 1.0, clip)
                                {
                                    legacy.push(RelocateRenderElement::from_element(
                                        element,
                                        shift,
                                        Relocate::Relative,
                                    ));
                                }
                            }
                        }

                        let elements = scene
                            .drain(..)
                            .filter_map(|element| {
                                let SceneElement::Shadow(element) = element else {
                                    unreachable!()
                                };
                                CropRenderElement::from_element(element, 1.0, clip).map(|element| {
                                    RelocateRenderElement::from_element(
                                        element,
                                        shift,
                                        Relocate::Relative,
                                    )
                                })
                            })
                            .collect::<Vec<_>>();
                        let target_size = transform.transform_size(scene_size);
                        let pixels = read_pixels(&mut renderer, &elements, target_size, transform);
                        let previous_pixels =
                            read_pixels(&mut renderer, &legacy, target_size, transform);
                        for (index, (&actual, &previous)) in
                            pixels.iter().zip(&previous_pixels).enumerate()
                        {
                            assert!(
                            actual.abs_diff(previous) <= 1,
                            "batch changed old tile pixels: size={authored_size:?} scale={presentation_scale} {transform:?} crop={cropped} byte={index} {actual}!={previous}"
                        );
                        }
                        for y in (0..target_size.h).step_by(3) {
                            for x in (0..target_size.w).step_by(3) {
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
                                let visible = 1.0
                                    - DecorationShape {
                                        rect: scaled_rect,
                                        radius: (f64::from(shadow.radius) * presentation_scale)
                                            .round()
                                            as u16,
                                        border: 0,
                                        border_rgb: [0; 3],
                                    }
                                    .coverage(at);
                                let expected =
                                    if clip.contains((at.x, at.y)) && scaled_bounds.contains(at) {
                                        crate::shadow_cache::gaussian(
                                            f64::from(at.x - scaled_rect.pos.x) + 0.5
                                                - f64::from(shadow.offset.x) * presentation_scale,
                                            f64::from(at.y - scaled_rect.pos.y) + 0.5
                                                - f64::from(shadow.offset.y) * presentation_scale,
                                            f64::from(scaled_rect.size.w),
                                            f64::from(scaled_rect.size.h),
                                            f64::from(shadow.radius) * presentation_scale,
                                            f64::from(shadow.blur) * 0.5 * presentation_scale,
                                        ) * f64::from(visible)
                                            * f64::from(shadow.rgba[3])
                                    } else {
                                        0.0
                                    };
                                let actual = pixels[((y * target_size.w + x) * 4 + 3) as usize];
                                assert!(
                                presentation_scale != 1.0
                                    || (f64::from(actual) - expected).abs() <= 2.0,
                                "scale={presentation_scale} {transform:?} crop={cropped} at={at:?} actual={actual} expected={expected}"
                            );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "requires GLES/EGL; checks composed modern border antialias coverage"]
    fn native_modern_composed_lower_border_has_single_coverage() {
        use smithay::backend::{
            allocator::Fourcc,
            egl::{native::EGLSurfacelessDisplay, EGLContext, EGLDisplay},
            renderer::{
                damage::OutputDamageTracker, element::solid::SolidColorRenderElement, Bind,
                Color32F, ExportMem, Offscreen,
            },
        };
        use wm_theme::{DecorationStyle, RasterThemeEngine};
        use wm_theme_api::{DecorationRequest, ThemeEngine};
        let theme = wm_theme::modern::theme("relay", wm_theme::Appearance::Dark).unwrap();
        let engine = RasterThemeEngine::new(theme)
            .with_style(DecorationStyle::Modern)
            .unwrap();
        let request = DecorationRequest {
            content_size: Size::new(320, 180),
            title: "Coverage oracle".into(),
            focused: true,
            resizable: true,
            buttons: Vec::new(),
        };
        let layout = engine.layout(&request);
        let surface = engine.render_surface(&request, &layout);
        let mut shape = surface.shape.unwrap();
        let origin = Point::new(20, 20);
        shape.rect.pos.x += origin.x;
        shape.rect.pos.y += origin.y;
        let mut inner = shape;
        let border = i32::from(shape.border);
        inner.rect.pos.x += border;
        inner.rect.pos.y += border;
        inner.rect.size.w -= 2 * border as u32;
        inner.rect.size.h -= 2 * border as u32;
        inner.radius = inner.radius.saturating_sub(shape.border);
        let content = PixelRect::new(
            (
                origin.x + layout.client_offset.x,
                origin.y + layout.client_offset.y,
            )
                .into(),
            (320, 180).into(),
        );
        // SAFETY: Only Smithay creates and terminates this test's surfaceless EGL display.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }.unwrap();
        let context = EGLContext::new(&display).unwrap();
        // SAFETY: This fresh context stays on the test thread and is not current elsewhere.
        let mut renderer = unsafe { GlesRenderer::new(context) }.unwrap();
        let bottom = wm_theme_api::Rect::new(
            Point::new(
                shape.rect.pos.x,
                shape.rect.pos.y + shape.rect.size.h as i32 - 1,
            ),
            Size::new(shape.rect.size.w, 1),
        );
        let mut retained = crate::state::FrameSolid::new(wm_theme_api::DecorationSolid {
            rect: bottom,
            rgb: shape.border_rgb,
        });
        retained.commit.increment();
        let mut probe = Vec::with_capacity(1);
        for alpha in [1.0_f32, 0.4] {
            for lower_border_drawn in [false, true] {
                probe.clear();
                probe.push(retained.element(bottom, alpha).into());
                crate::rounded::mask_frame_solids(
                    &mut probe,
                    0,
                    &mut renderer,
                    Some(shape),
                    lower_border_drawn,
                );
                let expected = if lower_border_drawn {
                    PixelRect::new(
                        (bottom.pos.x + i32::from(shape.radius), bottom.pos.y).into(),
                        (bottom.size.w as i32 - 2 * i32::from(shape.radius), 1).into(),
                    )
                } else {
                    PixelRect::new(
                        (bottom.pos.x, bottom.pos.y).into(),
                        (bottom.size.w as i32, 1).into(),
                    )
                };
                assert_eq!(probe[0].geometry(1.0.into()), expected);
                assert_eq!(probe[0].id(), &retained.id);
                assert_eq!(probe[0].current_commit(), retained.commit);
            }
        }
        let (_, cost) = chonk_test_support::measure(|| {
            for _ in 0..100 {
                probe.clear();
                probe.push(retained.element(bottom, 0.4).into());
                crate::rounded::mask_frame_solids(&mut probe, 0, &mut renderer, Some(shape), true);
            }
        });
        assert_eq!((cost.calls, cost.requested_bytes), (0, 0));
        let mut elements = vec![SceneElement::Solid(SolidColorRenderElement::new(
            Id::new(),
            content,
            CommitCounter::default(),
            Color32F::new(0.125, 0.25, 0.5, 1.0),
            Kind::Unspecified,
        ))];
        crate::rounded::mask_plane(&mut elements, 0, &mut renderer, Some(shape), true);
        let lower_border_drawn = crate::rounded::push_border(
            &mut elements,
            &mut renderer,
            Some(shape),
            &[Id::new(), Id::new()],
            CommitCounter::default(),
            1.0,
        );
        let start = elements.len();
        for solid in &surface.solids {
            let mut rect = solid.rect;
            rect.pos.x += origin.x;
            rect.pos.y += origin.y;
            elements.push(
                crate::state::FrameSolid::new(*solid)
                    .element(rect, 1.0)
                    .into(),
            );
        }
        assert!(lower_border_drawn);
        crate::rounded::mask_frame_solids(
            &mut elements,
            start,
            &mut renderer,
            Some(shape),
            lower_border_drawn,
        );
        let start = elements.len();
        elements.push(
            SolidColorRenderElement::new(
                Id::new(),
                content,
                CommitCounter::default(),
                Color32F::BLACK,
                Kind::Unspecified,
            )
            .into(),
        );
        crate::rounded::mask_plane(&mut elements, start, &mut renderer, Some(shape), true);
        let mut read = |background| {
            let mut texture: GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, (512, 384).into())
                .unwrap();
            let mut target = renderer.bind(&mut texture).unwrap();
            OutputDamageTracker::new((512, 384), 1.0, Transform::Normal)
                .render_output(&mut renderer, &mut target, 0, &elements, background)
                .unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((512, 384).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            let pixels = renderer.map_texture(&mapping).unwrap().to_vec();
            drop(mapping);
            drop(target);
            drop(texture);
            pixels
        };
        for background in [Color32F::TRANSPARENT, Color32F::new(0.2, 0.3, 0.4, 1.0)] {
            let pixels = read(background);
            let backdrop = [
                background.r(),
                background.g(),
                background.b(),
                background.a(),
            ];
            let radius = i32::from(shape.radius);
            let y0 = shape.rect.pos.y + shape.rect.size.h as i32 - radius;
            let mut inspected = 0;
            let mut worst = (0u8, 0u8, 0u8, Point::new(0, 0), 0usize);
            for x0 in [
                shape.rect.pos.x,
                shape.rect.pos.x + shape.rect.size.w as i32 - radius,
            ] {
                for y in y0..y0 + radius {
                    for x in x0..x0 + radius {
                        let point = Point::new(x, y);
                        let coverage = shape.coverage(point);
                        if inner.coverage(point) != 0.0 || coverage <= 0.0 || coverage >= 1.0 {
                            continue;
                        }
                        // Title parts cannot contribute to the lower-corner oracle.
                        assert!(surface
                            .parts
                            .iter()
                            .all(|part| y - origin.y >= part.offset.y + part.buffer.height as i32));
                        // Inner coverage is zero, so client/ring ordering cannot
                        // affect this single-border source-over oracle.
                        for channel in 0..4 {
                            let foreground = if channel == 3 {
                                255.0
                            } else {
                                f32::from(shape.border_rgb[channel])
                            };
                            let expected = (foreground * coverage
                                + backdrop[channel] * 255.0 * (1.0 - coverage))
                                .round() as u8;
                            let actual = pixels[((y * 512 + x) * 4) as usize + channel];
                            inspected += 1;
                            if actual.abs_diff(expected) > worst.0 {
                                worst =
                                    (actual.abs_diff(expected), actual, expected, point, channel);
                            }
                        }
                    }
                }
            }
            assert!(inspected > 0);
            assert!(worst.0<=1,"composed frame repeats corner coverage: worst difference {}, actual {}, analytic {}, at {:?} channel{} background{background:?}",worst.0,worst.1,worst.2,worst.3,worst.4);
        }
    }

    #[test]
    fn retained_effects_keep_identity_and_do_no_warm_allocation_or_input_expansion() {
        let rect = Rect::new(Point::new(5, 5), Size::new(802, 635));
        let shape = DecorationShape {
            rect,
            radius: 9,
            border: 1,
            border_rgb: [255; 3],
        };
        let shadow = DecorationShadow {
            rect,
            radius: 9,
            offset: Point::new(0, 10),
            blur: 30,
            rgba: [6, 9, 22, 112],
        };
        let mut surface = DecorationSurface {
            frame_size: Size::new(812, 645),
            parts: Vec::new(),
            solids: Vec::new(),
            shadow: Some(shadow),
            shape: Some(shape),
        };
        let mut retained = None;
        FrameEffects::update(&mut retained, &surface);
        let ids = retained.as_ref().unwrap().ids.clone();
        let commit = retained.as_ref().unwrap().commit;
        let (_, allocations) = chonk_test_support::measure(|| {
            for _ in 0..1000 {
                FrameEffects::update(&mut retained, &surface);
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
        let effects = retained.as_ref().unwrap();
        assert_eq!(effects.ids, ids);
        assert_eq!(effects.commit, commit);
        assert!(
            !effects.accepts_input(Point::new(5, 5)),
            "transparent corner exposes the scene beneath"
        );
        assert!(
            effects.accepts_input(Point::new(4, 4)),
            "existing invisible resize ring remains interactive"
        );
        assert!(effects.client_accepts_input(Point::new(30, 40)));
        assert!(!effects.client_accepts_input(Point::new(5, 630)));
        surface.shadow.as_mut().unwrap().rgba[0] += 1;
        FrameEffects::update(&mut retained, &surface);
        assert_eq!(retained.as_ref().unwrap().ids, ids);
        assert_ne!(retained.as_ref().unwrap().commit, commit);
        surface.shadow = None;
        surface.shape = None;
        FrameEffects::update(&mut retained, &surface);
        assert!(retained.is_none());
    }
}
