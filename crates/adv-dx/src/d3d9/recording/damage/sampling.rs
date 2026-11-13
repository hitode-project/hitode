//! Proven local texture reads, expressed in source texels and destination pixels.
use super::*;
use crate::shader::{
    Opcode, Register, RegisterKind, Shader, SourceModifier,
    interpreter::{self, Outputs},
};
use adv_device::gpu::{Address, Filter, Sampler, Topology};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::d3d9::recording) struct Mapping {
    /// Source texel coordinate = destination pixel coordinate * scale + offset.
    scale: [f64; 2],
    offset: [f64; 2],
    radius: f64,
    source: Rect,
    repeat: [bool; 2],
}
impl Mapping {
    fn new(scale: [f64; 2], offset: [f64; 2], radius: f64, image: &Image) -> Option<Self> {
        (scale.iter().all(|v| v.is_finite() && *v != 0.) && offset.iter().all(|v| v.is_finite()))
            .then_some(Self {
                scale,
                offset,
                radius,
                source: full(image),
                repeat: [false; 2],
            })
    }
    /// The same pixel of a `source` extent, as depth and stencil tests read.
    pub(in crate::d3d9::recording) fn identity(source: Rect) -> Self {
        Self {
            scale: [1.; 2],
            offset: [0.; 2],
            radius: 0.,
            source,
            repeat: [false; 2],
        }
    }
    /// Bounds the source texels that destination pixels in `pixels` read.
    pub(in crate::d3d9::recording) fn unmap(self, pixels: Rect) -> Rect {
        if pixels.empty() || self.source.empty() {
            return Rect::default();
        }
        let lo = [f64::from(pixels.x), f64::from(pixels.y)];
        let hi = [
            f64::from(pixels.x + pixels.w),
            f64::from(pixels.y + pixels.h),
        ];
        let first = [self.source.x, self.source.y];
        let last = [
            self.source.x + self.source.w - 1,
            self.source.y + self.source.h - 1,
        ];
        let mut start = [0; 2];
        let mut end = [0; 2];
        for n in 0..2 {
            let a = lo[n] * self.scale[n] + self.offset[n];
            let b = hi[n] * self.scale[n] + self.offset[n];
            let lo = (a.min(b) - self.radius).floor();
            let hi = (a.max(b) + self.radius).ceil() - 1.;
            let first = f64::from(first[n]);
            let last = f64::from(last[n]);
            // The two sides of a wrapped read share one bounding interval.
            let wraps = self.repeat[n] && (lo < first || hi > last);
            start[n] = if wraps { first } else { lo.clamp(first, last) } as u32;
            end[n] = if wraps { last } else { hi.clamp(first, last) } as u32 + 1;
        }
        Rect {
            x: start[0],
            y: start[1],
            w: end[0] - start[0],
            h: end[1] - start[1],
        }
    }
    pub(in crate::d3d9::recording) fn map(self, damage: Rect, bounds: Rect) -> Rect {
        let damage = damage.clipped(self.source);
        if damage.empty() {
            return Rect::default();
        }
        // Clamped addressing repeats edge texels without limit, so damage on
        // a source edge reaches every coordinate beyond it.
        let edge = |axis: usize, at: u32, limit: u32, value: f64, beyond: f64| {
            if !self.repeat[axis] && at == limit {
                beyond
            } else {
                value
            }
        };
        let source = self.source;
        let lo = [
            edge(
                0,
                damage.x,
                source.x,
                f64::from(damage.x) - self.radius,
                f64::NEG_INFINITY,
            ),
            edge(
                1,
                damage.y,
                source.y,
                f64::from(damage.y) - self.radius,
                f64::NEG_INFINITY,
            ),
        ];
        let hi = [
            edge(
                0,
                damage.x + damage.w,
                source.x + source.w,
                f64::from(damage.x + damage.w) + self.radius,
                f64::INFINITY,
            ),
            edge(
                1,
                damage.y + damage.h,
                source.y + source.h,
                f64::from(damage.y + damage.h) + self.radius,
                f64::INFINITY,
            ),
        ];
        let mut start = [0; 2];
        let mut end = [0; 2];
        let min = [bounds.x, bounds.y];
        let max = [bounds.x + bounds.w, bounds.y + bounds.h];
        let extent = [source.w, source.h];
        for n in 0..2 {
            start[n] = max[n];
            end[n] = min[n];
            // The proven coordinate range and filter reach at most the two
            // adjacent periods (one-texel axes are equivalent to clamping).
            let periods = i32::from(self.repeat[n]);
            for period in -periods..=periods {
                let shift = f64::from(period) * f64::from(extent[n]);
                let a = (lo[n] + shift - self.offset[n]) / self.scale[n];
                let b = (hi[n] + shift - self.offset[n]) / self.scale[n];
                let lo = (a.min(b).floor() as u32).max(min[n]);
                let hi = (a.max(b).ceil() as u32).min(max[n]);
                if lo < hi {
                    start[n] = start[n].min(lo);
                    end[n] = end[n].max(hi);
                }
            }
        }
        Rect {
            x: start[0],
            y: start[1],
            w: end[0].saturating_sub(start[0]),
            h: end[1].saturating_sub(start[1]),
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(in crate::d3d9::recording) struct Read {
    pub source: Content,
    pub mapping: Mapping,
}

/// No mapping is inferred from arbitrary shader output or from sampled values.
pub(in crate::d3d9::recording) fn prepare(
    env: &Env<'_>,
    device: &Device,
    operation: &Operation,
    reads: &[Content],
) -> Vec<Read> {
    if !reads.iter().any(|k| device.frame.damage.written(k)) {
        return Vec::new();
    }
    let result = match operation {
        Operation::Copy {
            source,
            rectangle,
            region,
            filter,
            ..
        } if reads.len() == 1 => Mapping::new(
            [
                f64::from(rectangle.w) / f64::from(region.w),
                f64::from(rectangle.h) / f64::from(region.h),
            ],
            [
                f64::from(rectangle.x)
                    - f64::from(region.x) * f64::from(rectangle.w) / f64::from(region.w),
                f64::from(rectangle.y)
                    - f64::from(region.y) * f64::from(rectangle.h) / f64::from(region.h),
            ],
            if *filter == Filter::Linear { 0.5 } else { 0. },
            source,
        )
        .map(|mapping| {
            vec![Read {
                source: reads[0],
                mapping,
            }]
        }),
        Operation::Draw(draw) => draw_reads(env, device, draw)
            .inspect_err(|why| super::super::trace::unmapped(why))
            .ok(),
        _ => None,
    };
    result.unwrap_or_default()
}

/// One varying component as `scale * varying + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Coordinate {
    varying: usize,
    component: usize,
    scale: f64,
    offset: f64,
}
impl Coordinate {
    fn varying(varying: usize, component: usize) -> Self {
        Self {
            varying,
            component,
            scale: 1.,
            offset: 0.,
        }
    }
    fn affine(self, scale: f64, offset: f64) -> Self {
        Self {
            scale: self.scale * scale,
            offset: self.offset * scale + offset,
            ..self
        }
    }
}

/// Track varyings through MOV, ADD, MUL and MAD with constants as affine
/// forms; other arithmetic and texture results cannot become a coordinate
/// proof. Other pixel arithmetic may consume samples locally. `constant`
/// reads float constant registers the shader does not define.
fn coordinates(
    shader: &Shader,
    vertex: Option<&Shader>,
    constant: &dyn Fn(usize) -> [f32; 4],
) -> Option<Vec<(usize, [Coordinate; 2])>> {
    use Opcode::*;
    if shader.version.major < 2 {
        return None;
    }
    let mut inputs = [[None; 4]; 16];
    let mut textures = [[None; 4]; 8];
    let mut temporary = [[None; 4]; 32];
    for (n, components) in textures.iter_mut().enumerate() {
        for (c, value) in components.iter_mut().enumerate() {
            *value = Some(Coordinate::varying(2 + n, c));
        }
    }
    for (n, input) in inputs.iter_mut().take(2).enumerate() {
        for (c, component) in input.iter_mut().enumerate() {
            *component = Some(Coordinate::varying(n, c));
        }
    }
    if shader.version.major >= 3 {
        inputs = [[None; 4]; 16];
        for d in shader
            .inputs
            .iter()
            .filter(|d| d.register.kind == RegisterKind::Input)
        {
            let varying = if vertex.is_some_and(|v| v.version.major == 3) {
                usize::from(d.register.index)
            } else {
                match (d.usage, d.index) {
                    (10, n @ 0..=1) => usize::from(n),
                    (5, n @ 0..=7) => 2 + usize::from(n),
                    _ => return None,
                }
            };
            if varying >= 10 {
                return None;
            }
            for c in 0..4 {
                inputs.get_mut(usize::from(d.register.index))?[c] =
                    Some(Coordinate::varying(varying, c));
            }
        }
    }
    let defined: Vec<(u16, [f32; 4])> = shader
        .instructions
        .iter()
        .filter(|i| i.opcode == Def)
        .filter_map(|i| {
            let d = i.destination.as_ref()?;
            let value: [u32; 4] = i.immediate.as_slice().try_into().ok()?;
            (d.register.kind == RegisterKind::Float)
                .then(|| (d.register.index, value.map(f32::from_bits)))
        })
        .collect();
    let mut samples = Vec::new();
    for instruction in &shader.instructions {
        if instruction.predicate.is_some()
            || instruction.coissue
            || instruction.destination.as_ref().is_some_and(|d| d.centroid)
        {
            return None;
        }
        if matches!(
            instruction.opcode,
            Call | CallNz
                | Loop
                | Ret
                | EndLoop
                | Label
                | Rep
                | EndRep
                | If
                | Ifc
                | Else
                | EndIf
                | Break
                | Breakc
                | Breakp
                | Dsx
                | Dsy
                | Texldd
                | Texldl
                | Texbem
                | Texbeml
                | Texreg2ar
                | Texreg2gb
                | Texm3x2pad
                | Texm3x2tex
                | Texm3x3pad
                | Texm3x3tex
                | Texm3x3spec
                | Texm3x3vspec
                | Texreg2rgb
                | Texdp3tex
                | Texm3x2depth
                | Texdp3
                | Texm3x3
                | Texdepth
                | Texcoord
                | Phase
        ) {
            return None;
        }
        if matches!(instruction.opcode, Nop | Dcl | Def | Defi | Defb | Texkill) {
            continue;
        }
        let read = |register: Register| match register.kind {
            RegisterKind::Temp => temporary.get(usize::from(register.index)).copied(),
            RegisterKind::Input => inputs.get(usize::from(register.index)).copied(),
            RegisterKind::Address => textures.get(usize::from(register.index)).copied(),
            _ => None,
        };
        // A source as affine forms, or as constant values; NEG keeps either.
        let negated = |s: &crate::shader::Source| match s.modifier {
            SourceModifier::None => Some(1.),
            SourceModifier::Negate => Some(-1.),
            _ => None,
        };
        let affine = |s: &crate::shader::Source| -> [Option<Coordinate>; 4] {
            let (Some(sign), None) = (negated(s), s.relative) else {
                return [None; 4];
            };
            read(s.register).map_or([None; 4], |values| {
                s.swizzle
                    .map(|c| values[usize::from(c)].map(|v| v.affine(sign, 0.)))
            })
        };
        let constant = |s: &crate::shader::Source| -> Option<[f64; 4]> {
            let (Some(sign), None) = (negated(s), s.relative) else {
                return None;
            };
            if s.register.kind != RegisterKind::Float {
                return None;
            }
            let n = s.register.index;
            let value = defined
                .iter()
                .rev()
                .find(|(r, _)| *r == n)
                .map_or_else(|| constant(usize::from(n)), |(_, v)| *v);
            Some(s.swizzle.map(|c| sign * f64::from(value[usize::from(c)])))
        };
        let sources = &instruction.sources;
        let source = sources.first().map(affine);
        if instruction.opcode == Tex {
            if instruction.controls != 0 {
                return None;
            }
            let sampler = instruction.sources.get(1)?;
            if sampler.register.kind != RegisterKind::Sampler || sampler.relative.is_some() {
                return None;
            }
            let source = source?;
            samples.push((
                usize::from(sampler.register.index),
                [source[0]?, source[1]?],
            ));
        }
        let destination = instruction.destination.as_ref()?;
        if destination.relative.is_some() {
            return None;
        }
        let plain =
            !destination.saturate && !destination.partial_precision && destination.shift == 0;
        // value * scale + offset with one affine operand and constant others.
        let combine = |value: [Option<Coordinate>; 4],
                       scale: Option<[f64; 4]>,
                       offset: Option<[f64; 4]>| {
            std::array::from_fn(|c| {
                value[c].map(|v| v.affine(scale.map_or(1., |s| s[c]), offset.map_or(0., |o| o[c])))
            })
        };
        let either = |a: usize, b: usize| {
            let (x, y) = (sources.get(a)?, sources.get(b)?);
            match (constant(x), constant(y)) {
                (None, Some(k)) => Some((affine(x), k)),
                (Some(k), None) => Some((affine(y), k)),
                _ => None,
            }
        };
        let moved = match instruction.opcode {
            _ if !plain => [None; 4],
            Mov => source.unwrap_or([None; 4]),
            Add => either(0, 1).map_or([None; 4], |(v, k)| combine(v, None, Some(k))),
            Mul => either(0, 1).map_or([None; 4], |(v, k)| combine(v, Some(k), None)),
            Mad => match (either(0, 1), sources.get(2).and_then(constant)) {
                (Some((v, k)), Some(o)) => combine(v, Some(k), Some(o)),
                _ => [None; 4],
            },
            _ => [None; 4],
        };
        let values = match destination.register.kind {
            RegisterKind::Temp => temporary.get_mut(usize::from(destination.register.index)),
            RegisterKind::Address => textures.get_mut(usize::from(destination.register.index)),
            _ => None,
        };
        if let Some(values) = values {
            for c in 0..4 {
                if destination.mask & (1 << c) != 0 {
                    values[c] = moved[c];
                }
            }
        }
    }
    Some(samples)
}

fn draw_reads(
    env: &Env<'_>,
    device: &Device,
    draw: &fixed::Draw,
) -> Result<Vec<Read>, &'static str> {
    if draw.programmable.as_ref().is_some_and(|p| {
        p.instance_count != 1 || p.parameters[15][0] == 2 || p.streams.iter().any(|s| s.instance)
    }) {
        return Err("instanced draw");
    }
    if !matches!(
        draw.state.topology,
        Topology::Triangles | Topology::TriangleStrip
    ) {
        return Err("not a triangle list or strip");
    }
    // Neutral vertices mean POSITIONT bypassed the bound vertex shader.
    let vertex = device
        .shaders
        .vertex
        .filter(|_| {
            draw.vertices.is_empty()
                && draw
                    .programmable
                    .as_ref()
                    .is_none_or(|p| p.parameters[15][0] == 0)
        })
        .and_then(|id| shaders::bound(env, id));
    if vertex.is_some_and(|v| {
        v.translation
            .as_ref()
            .ok()
            .is_none_or(|t| !t.samplers.is_empty())
    }) {
        return Err("vertex shader samples or is untranslated");
    }
    let pixel = device.shaders.pixel.and_then(|id| shaders::bound(env, id));
    let samples = if let Some(pixel) = pixel {
        let constants = draw.programmable.as_ref().map(|p| &p.pixel_constants);
        let constant = |n: usize| constants.map_or([0.; 4], |c| c.register(n).map(f32::from_bits));
        coordinates(&pixel.parsed, vertex.map(|v| v.parsed.as_ref()), &constant).ok_or_else(
            || {
                if super::super::trace::capturing() {
                    super::super::trace::detail(unmapped_detail(draw, vertex, pixel));
                }
                "pixel shader coordinates are not plain varyings"
            },
        )?
    } else {
        if draw
            .programmable
            .as_ref()
            .is_some_and(|p| p.textures.len() > 1)
        {
            return Err("several textures without a pixel shader");
        }
        vec![(0, [Coordinate::varying(2, 0), Coordinate::varying(2, 1)])]
    };
    if vertex.is_some_and(|v| v.translation.as_ref().is_err()) {
        return Err("vertex shader is untranslated");
    }
    let vertices = vertices(
        draw,
        vertex.map(|v| v.parsed.as_ref()),
        pixel.map(|p| p.parsed.as_ref()),
    )
    .ok_or("vertices are not 3 to 64 with one positive w")?;
    let mut result = Vec::new();
    for (slot, coordinates) in samples {
        let binding = if let Some(p) = &draw.programmable {
            let ordinal = if let Some(pixel) = pixel {
                pixel
                    .translation
                    .as_ref()
                    .map_err(|_| "pixel shader is untranslated")?
                    .samplers
                    .iter()
                    .position(|s| usize::from(s.index) == slot)
                    .ok_or("sampler is not translated")?
            } else {
                0
            };
            let texture = p.textures.get(ordinal).ok_or("texture is not bound")?;
            (
                texture.image.as_ref().ok_or("texture has no image")?,
                texture.sampler,
            )
        } else {
            (
                draw.image.as_ref().ok_or("texture has no image")?,
                draw.state.sampler,
            )
        };
        let resource = if slot == 0 {
            device.texture
        } else {
            *device
                .shaders
                .textures
                .get(slot)
                .ok_or("texture is not bound")?
        }
        .ok_or("texture is not bound")?;
        let mapping = mapping(&vertices, coordinates, binding.0, binding.1).ok_or_else(|| {
            if super::super::trace::capturing() {
                let coordinates: Vec<_> = vertices
                    .iter()
                    .map(|v| {
                        let uv = coordinates.map(|c| {
                            f64::from(v.varyings[c.varying][c.component]) * c.scale + c.offset
                        });
                        (v.position, uv)
                    })
                    .collect();
                super::super::trace::detail(format!(
                    "    sampler {:?}: {coordinates:?}",
                    binding.1
                ));
            }
            "texels are not an axis-aligned affine map of pixels"
        })?;
        result.push(Read {
            source: key::content(env, resource, Some(0), 0),
            mapping,
        });
    }
    Ok(result)
}

/// What the live debugger shows for a draw whose coordinates are not
/// proven: both programs, the vertices as positioned, and the textures.
fn unmapped_detail(
    draw: &fixed::Draw,
    vertex: Option<&crate::d3d9::shaders::Shader>,
    pixel: &crate::d3d9::shaders::Shader,
) -> String {
    let mut text = vec![format!(
        "    pixel {}",
        super::super::trace::program(&pixel.parsed)
    )];
    text.push(match vertex {
        Some(v) => format!("    vertex {}", super::super::trace::program(&v.parsed)),
        None => format!(
            "    vertex: none, transformed {}",
            draw.programmable
                .as_ref()
                .is_some_and(|p| p.parameters[15][0] != 0)
        ),
    });
    match vertices(draw, vertex.map(|v| v.parsed.as_ref()), Some(&pixel.parsed)) {
        Some(vertices) => {
            for v in &vertices {
                text.push(format!(
                    "    vertex at {:?}: t0 {:?} t1 {:?}",
                    v.position, v.varyings[2], v.varyings[3]
                ));
            }
        }
        None => text.push("    vertices: not positioned".into()),
    }
    for t in draw.programmable.iter().flat_map(|p| &p.textures) {
        text.push(format!(
            "    texture {:?} {:?}",
            t.image.as_ref().map(|i| (i.width(), i.height())),
            t.sampler
        ));
    }
    text.join("\n")
}
struct Vertex {
    position: [f64; 2],
    varyings: [[f32; 4]; 10],
}
fn vertices(
    draw: &fixed::Draw,
    vertex: Option<&Shader>,
    pixel: Option<&Shader>,
) -> Option<Vec<Vertex>> {
    let transformed = draw
        .programmable
        .as_ref()
        .is_some_and(|p| p.parameters[15][0] != 0);
    let count = if vertex.is_none() && !transformed {
        draw.vertices.len()
    } else {
        draw.programmable
            .as_ref()
            .map_or(0, |p| p.vertex_count as usize)
    };
    if !(3..=64).contains(&count) {
        return None;
    }
    let mut result = Vec::new();
    let mut common_w = None;
    for n in 0..count {
        let mut output = if let Some(vertex) = vertex {
            let program = draw.programmable.as_ref()?;
            interpreter::run(
                vertex,
                &vertex_input(program, n as u32)?,
                &program.vertex_constants,
                &mut interpreter::NoTextures,
            )
            .ok()?
        } else if transformed {
            let p = draw.programmable.as_ref()?;
            let input = vertex_input(p, n as u32)?;
            let mut output = Outputs {
                position: programmable::transformed::position(input.v[0], draw.state.viewport)?,
                colors: [[1.; 4], [0.; 4]],
                ..Default::default()
            };
            for stream in &p.streams {
                for a in &stream.attributes {
                    match a.location {
                        1 => output.colors[0] = input.v[1],
                        n @ 2..=9 => output.texcoords[(n - 2) as usize] = input.v[n as usize],
                        _ => {}
                    }
                }
            }
            output
        } else {
            let v = draw.vertices.get(n)?;
            let mut output = Outputs {
                position: v.position,
                colors: [v.color, [0.; 4]],
                ..Default::default()
            };
            output.texcoords[0] = [v.uv[0], v.uv[1], 0., 1.];
            if let Some((count, values)) = &draw.texcoords {
                let start = n * count;
                output.texcoords[..*count].copy_from_slice(values.get(start..start + count)?);
            }
            output
        };
        let w = output.position[3];
        if !w.is_finite() || w <= 0. || common_w.is_some_and(|old| old != w) {
            return None;
        }
        common_w = Some(w);
        let viewport = draw.state.viewport;
        if vertex.is_some() {
            output.position[0] += w / viewport.width;
            output.position[1] -= w / viewport.height;
        }
        let position = [
            f64::from(viewport.x + (output.position[0] / w + 1.) * viewport.width * 0.5),
            f64::from(viewport.y + (1. - output.position[1] / w) * viewport.height * 0.5),
        ];
        if position.iter().any(|v| !v.is_finite()) {
            return None;
        }
        let mut varyings = std::array::from_fn(|i| {
            if i < 2 {
                output.colors[i]
            } else {
                output.texcoords[i - 2]
            }
        });
        if let (Some(vertex), Some(pixel)) = (vertex, pixel)
            && vertex.version.major == 3
            && pixel.version.major == 3
        {
            for input in pixel
                .inputs
                .iter()
                .filter(|d| d.register.kind == RegisterKind::Input)
            {
                let output_register = vertex
                    .outputs
                    .iter()
                    .find(|o| o.usage == input.usage && o.index == input.index)?;
                *varyings.get_mut(usize::from(input.register.index))? = *output
                    .generic
                    .get(usize::from(output_register.register.index))?;
            }
        }
        result.push(Vertex { position, varyings });
    }
    Some(result)
}

fn mapping(
    vertices: &[Vertex],
    coordinates: [Coordinate; 2],
    image: &Image,
    sampler: Sampler,
) -> Option<Mapping> {
    let address = [sampler.u, sampler.v];
    if sampler.first_level != 0 || sampler.mip.is_some() || address.contains(&Address::Border) {
        return None;
    }
    let mut scale = [0.; 2];
    let mut offset = [0.; 2];
    let mut residual = 0.0f64;
    for axis in 0..2 {
        let low = vertices
            .iter()
            .min_by(|a, b| a.position[axis].total_cmp(&b.position[axis]))?;
        let high = vertices
            .iter()
            .max_by(|a, b| a.position[axis].total_cmp(&b.position[axis]))?;
        if low.position[axis] == high.position[axis] {
            return None;
        }
        let coordinate = coordinates[axis];
        let value = |v: &Vertex| {
            f64::from(v.varyings[coordinate.varying][coordinate.component]) * coordinate.scale
                + coordinate.offset
        };
        let a = value(low);
        let b = value(high);
        let size = f64::from(if axis == 0 {
            image.width()
        } else {
            image.height()
        });
        // The proof covers the source and one texel past each edge, including
        // repeat samples from the adjacent periods.
        if !a.is_finite() || !b.is_finite() || a.min(b) * size < -1. || a.max(b) * size > size + 1.
        {
            return None;
        }
        scale[axis] = (b - a) * size / (high.position[axis] - low.position[axis]);
        offset[axis] = a * size - low.position[axis] * scale[axis];
        // Equal homogeneous w makes the interpolation error no larger than
        // its largest vertex error. Allow rounding in tessellated rectangles.
        for v in vertices {
            let texel = value(v) * size;
            let error = (texel - (v.position[axis] * scale[axis] + offset[axis])).abs();
            if !(0. ..=1. / 256.).contains(&error) || !(-1. ..=size + 1.).contains(&texel) {
                return None;
            }
            // In the base period, a linear mirror tap one texel across the
            // edge reads the same edge texel as clamp.
            if address[axis] == Address::Mirror && !(0. ..=size).contains(&texel) {
                return None;
            }
            residual = residual.max(error);
        }
    }
    let mut mapping = Mapping::new(
        scale,
        offset,
        residual
            + if sampler.min == Filter::Linear || sampler.mag == Filter::Linear {
                0.5
            } else {
                0.
            },
        image,
    )?;
    mapping.repeat = [
        sampler.u == Address::Repeat && image.width() > 1,
        sampler.v == Address::Repeat && image.height() > 1,
    ];
    Some(mapping)
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-REC-4
    #[test]
    fn constant_arithmetic_keeps_coordinates_affine() {
        // texld r0, t0, s0; mov oC0, r0
        let shader = Shader::parse(&[
            0xffff0200, 0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800,
            0x80e40000, 0xffff,
        ])
        .unwrap();
        let register = |kind, index| Register { kind, index };
        let temp = register(RegisterKind::Temp, 1);
        let with = |opcode, operands: &[Register]| {
            // OP r1, operands...; texld r0, r1, s0; mov oC0, r0
            let mut s = shader.clone();
            let mut op = s.instructions[0].clone();
            op.opcode = opcode;
            op.destination.as_mut().unwrap().register = temp;
            op.sources = operands
                .iter()
                .map(|&r| crate::shader::Source {
                    register: r,
                    ..shader.instructions[0].sources[0].clone()
                })
                .collect();
            s.instructions.insert(0, op);
            s.instructions[1].sources[0].register = temp;
            s
        };
        let t0 = shader.instructions[0].sources[0].register;
        let c = |n| register(RegisterKind::Float, n);
        let constant = |n: usize| [[0.25, 0.5, 0., 0.], [2., 4., 0., 0.]][n];
        let proof = |s: &Shader| coordinates(s, None, &constant).map(|v| v[0].1);
        let affine = |scale: f64, offset: f64| Coordinate {
            scale,
            offset,
            ..Coordinate::varying(2, 0)
        };
        let [x, y] = proof(&with(Opcode::Add, &[t0, c(0)])).unwrap();
        assert_eq!(x, affine(1., 0.25));
        assert_eq!(
            y,
            Coordinate {
                offset: 0.5,
                ..Coordinate::varying(2, 1)
            }
        );
        assert_eq!(
            proof(&with(Opcode::Add, &[c(0), t0])).unwrap()[0],
            affine(1., 0.25)
        );
        assert_eq!(
            proof(&with(Opcode::Mul, &[t0, c(1)])).unwrap()[0],
            affine(2., 0.)
        );
        assert_eq!(
            proof(&with(Opcode::Mad, &[t0, c(1), c(0)])).unwrap()[0],
            affine(2., 0.25)
        );
        // Two varyings, or a varying times itself, are not one affine form.
        assert!(proof(&with(Opcode::Add, &[t0, t0])).is_none());
        assert!(proof(&with(Opcode::Mul, &[t0, t0])).is_none());
        // A shader DEF wins over the draw's constant.
        let mut defined = with(Opcode::Add, &[t0, c(0)]);
        let mut def = defined.instructions[0].clone();
        def.opcode = Opcode::Def;
        def.destination.as_mut().unwrap().register = c(0);
        def.sources.clear();
        def.immediate = [1f32, 1., 0., 0.].map(f32::to_bits).to_vec();
        defined.instructions.insert(0, def);
        assert_eq!(proof(&defined).unwrap()[0], affine(1., 1.));
    }
    // spec: GFX-REC-4
    #[test]
    fn only_direct_coordinates_preserve_a_local_sampling_proof() {
        let shader = Shader::parse(&[
            0xffff0200, 0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800,
            0x80e40000, 0xffff,
        ])
        .unwrap();
        assert_eq!(coordinates(&shader, None, &|_| [0.; 4]).unwrap().len(), 1);
        let mut moved = shader.clone();
        let mut mov = shader.instructions[1].clone();
        mov.destination.as_mut().unwrap().register = Register {
            kind: RegisterKind::Temp,
            index: 1,
        };
        mov.sources[0] = shader.instructions[0].sources[0].clone();
        moved.instructions.insert(0, mov);
        moved.instructions[1].sources[0].register = Register {
            kind: RegisterKind::Temp,
            index: 1,
        };
        assert_eq!(
            coordinates(&moved, None, &|_| [0.; 4]),
            coordinates(&shader, None, &|_| [0.; 4])
        );
        let mut dependent = shader.clone();
        let mut next = dependent.instructions[0].clone();
        next.sources[0].register = Register {
            kind: RegisterKind::Temp,
            index: 0,
        };
        dependent.instructions.insert(1, next);
        assert!(coordinates(&dependent, None, &|_| [0.; 4]).is_none());
        for opcode in [Opcode::Dsx, Opcode::Dsy, Opcode::Texldd, Opcode::If] {
            let mut nonlocal = shader.clone();
            nonlocal.instructions[1].opcode = opcode;
            assert!(coordinates(&nonlocal, None, &|_| [0.; 4]).is_none());
        }
        let mut centroid = shader.clone();
        centroid.instructions[0]
            .destination
            .as_mut()
            .unwrap()
            .centroid = true;
        assert!(coordinates(&centroid, None, &|_| [0.; 4]).is_none());
        for controls in [1, 2] {
            let mut projected = shader.clone();
            projected.instructions[0].controls = controls;
            assert!(coordinates(&projected, None, &|_| [0.; 4]).is_none());
        }
    }
    // spec: GFX-REC-4
    #[test]
    fn clamped_edges_reach_every_pixel_beyond_the_source() {
        // Four texels magnified 32 times and shifted left: the leftmost
        // destination pixels read the clamped edge texel.
        let m = Mapping {
            scale: [1. / 32., 1. / 32.],
            offset: [-0.75, -0.75],
            radius: 0.5,
            repeat: [false; 2],
            source: Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 3,
            },
        };
        let bounds = Rect {
            x: 0,
            y: 0,
            w: 128,
            h: 96,
        };
        let edge = Rect {
            x: 0,
            y: 1,
            w: 1,
            h: 1,
        };
        assert_eq!(m.map(edge, bounds).x, 0);
        let right = Rect { x: 3, ..edge };
        let r = m.map(right, bounds);
        assert_eq!(r.x + r.w, 128);
        let corner = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        };
        assert_eq!(
            m.unmap(corner),
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1
            }
        );
    }

    // spec: GFX-REC-4 GFX-REC-5
    #[test]
    fn linear_repeat_reaches_the_opposite_edge() {
        let m = Mapping {
            scale: [1.; 2],
            offset: [-0.25, 0.],
            radius: 0.5,
            repeat: [true, false],
            source: Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 8,
            },
        };
        let bounds = Rect { w: 6, ..m.source };
        let edge = Rect {
            x: 7,
            y: 2,
            w: 1,
            h: 1,
        };
        let changed = m.map(edge, bounds);
        assert_eq!((changed.x, changed.w), (0, 1));
        let read = m.unmap(changed);
        assert_eq!((read.x, read.w), (0, 8));
        let middle = m.map(Rect { x: 2, ..edge }, bounds);
        assert_eq!((middle.x, middle.w), (1, 3));
    }
}
