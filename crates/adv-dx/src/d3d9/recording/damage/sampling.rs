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
}
impl Mapping {
    fn new(scale: [f64; 2], offset: [f64; 2], radius: f64, image: &Image) -> Option<Self> {
        (scale.iter().all(|v| v.is_finite() && *v != 0.) && offset.iter().all(|v| v.is_finite()))
            .then_some(Self {
                scale,
                offset,
                radius,
                source: full(image),
            })
    }
    pub(in crate::d3d9::recording) fn map(self, damage: Rect, bounds: Rect) -> Rect {
        let damage = damage.clipped(self.source);
        if damage.empty() {
            return Rect::default();
        }
        let lo = [
            f64::from(damage.x) - self.radius,
            f64::from(damage.y) - self.radius,
        ];
        let hi = [
            f64::from(damage.x + damage.w) + self.radius,
            f64::from(damage.y + damage.h) + self.radius,
        ];
        let mut start = [0; 2];
        let mut end = [0; 2];
        for n in 0..2 {
            let a = (lo[n] - self.offset[n]) / self.scale[n];
            let b = (hi[n] - self.offset[n]) / self.scale[n];
            // Outward rounding includes the sampler footprint at fractional edges.
            start[n] = a.min(b).floor().max(0.) as u32;
            end[n] = a.max(b).ceil().max(0.) as u32;
        }
        Rect {
            x: start[0],
            y: start[1],
            w: end[0].saturating_sub(start[0]),
            h: end[1].saturating_sub(start[1]),
        }
        .clipped(bounds)
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
        Operation::Draw(draw) => draw_reads(env, device, draw),
        _ => None,
    };
    result.unwrap_or_default()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Coordinate {
    varying: usize,
    component: usize,
}

/// Track unmodified varyings through MOVs; arithmetic and texture results cannot
/// become a coordinate proof. Other pixel arithmetic may consume samples locally.
fn coordinates(shader: &Shader, vertex: Option<&Shader>) -> Option<Vec<(usize, [Coordinate; 2])>> {
    use Opcode::*;
    if shader.version.major < 2 {
        return None;
    }
    let mut inputs = [[None; 4]; 16];
    let mut textures = [[None; 4]; 8];
    let mut temporary = [[None; 4]; 32];
    for (n, components) in textures.iter_mut().enumerate() {
        for (c, value) in components.iter_mut().enumerate() {
            *value = Some(Coordinate {
                varying: 2 + n,
                component: c,
            });
        }
    }
    for (n, input) in inputs.iter_mut().take(2).enumerate() {
        for (c, component) in input.iter_mut().enumerate() {
            *component = Some(Coordinate {
                varying: n,
                component: c,
            });
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
                inputs.get_mut(usize::from(d.register.index))?[c] = Some(Coordinate {
                    varying,
                    component: c,
                });
            }
        }
    }
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
        let source = instruction.sources.first().and_then(|s| {
            (s.modifier == SourceModifier::None && s.relative.is_none())
                .then(|| read(s.register).map(|values| s.swizzle.map(|c| values[usize::from(c)])))
                .flatten()
        });
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
        let moved = if instruction.opcode == Mov
            && !destination.saturate
            && !destination.partial_precision
            && destination.shift == 0
        {
            source.unwrap_or([None; 4])
        } else {
            [None; 4]
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

fn draw_reads(env: &Env<'_>, device: &Device, draw: &fixed::Draw) -> Option<Vec<Read>> {
    if !matches!(
        draw.state.topology,
        Topology::Triangles | Topology::TriangleStrip
    ) {
        return None;
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
        return None;
    }
    let pixel = device.shaders.pixel.and_then(|id| shaders::bound(env, id));
    let samples = if let Some(pixel) = pixel {
        coordinates(&pixel.parsed, vertex.map(|v| v.parsed.as_ref()))?
    } else {
        if draw
            .programmable
            .as_ref()
            .is_some_and(|p| p.textures.len() > 1)
        {
            return None;
        }
        vec![(
            0,
            [
                Coordinate {
                    varying: 2,
                    component: 0,
                },
                Coordinate {
                    varying: 2,
                    component: 1,
                },
            ],
        )]
    };
    if vertex.is_some_and(|v| v.translation.as_ref().is_err()) {
        return None;
    }
    let vertices = vertices(
        draw,
        vertex.map(|v| v.parsed.as_ref()),
        pixel.map(|p| p.parsed.as_ref()),
    )?;
    let mut result = Vec::new();
    for (slot, coordinates) in samples {
        let binding = if let Some(p) = &draw.programmable {
            let ordinal = if let Some(pixel) = pixel {
                pixel
                    .translation
                    .as_ref()
                    .ok()?
                    .samplers
                    .iter()
                    .position(|s| usize::from(s.index) == slot)?
            } else {
                0
            };
            let texture = p.textures.get(ordinal)?;
            (texture.image.as_ref()?, texture.sampler)
        } else {
            (draw.image.as_ref()?, draw.state.sampler)
        };
        let resource = if slot == 0 {
            device.texture
        } else {
            *device.shaders.textures.get(slot)?
        }?;
        let mapping = mapping(&vertices, coordinates, binding.0, binding.1)?;
        result.push(Read {
            source: key::content(env, resource, Some(0), 0),
            mapping,
        });
    }
    Some(result)
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
                for c in 0..*count {
                    output.texcoords[c] = *values.get(n * count + c)?;
                }
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
    if sampler.first_level != 0
        || sampler.mip.is_some()
        || sampler.u != Address::Clamp
        || sampler.v != Address::Clamp
    {
        return None;
    }
    let mut scale = [0.; 2];
    let mut offset = [0.; 2];
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
        let value = |v: &Vertex| v.varyings[coordinate.varying][coordinate.component];
        let a = value(low);
        let b = value(high);
        if !a.is_finite() || !b.is_finite() || a.min(b) < 0. || a.max(b) > 1. {
            return None;
        }
        // Two coordinate planes prove one global axis-aligned interpolation,
        // including indexed quads and triangle strips, without fitting a shader.
        for v in vertices {
            if !((v.position[axis] == low.position[axis] && value(v) == a)
                || (v.position[axis] == high.position[axis] && value(v) == b))
            {
                return None;
            }
        }
        let size = f64::from(if axis == 0 {
            image.width()
        } else {
            image.height()
        });
        scale[axis] = f64::from(b - a) * size / (high.position[axis] - low.position[axis]);
        offset[axis] = f64::from(a) * size - low.position[axis] * scale[axis];
    }
    Mapping::new(
        scale,
        offset,
        if sampler.min == Filter::Linear || sampler.mag == Filter::Linear {
            0.5
        } else {
            0.
        },
        image,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    // spec: GFX-REC-4
    #[test]
    fn only_direct_coordinates_preserve_a_local_sampling_proof() {
        let shader = Shader::parse(&[
            0xffff0200, 0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800,
            0x80e40000, 0xffff,
        ])
        .unwrap();
        assert_eq!(coordinates(&shader, None).unwrap().len(), 1);
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
        assert_eq!(coordinates(&moved, None), coordinates(&shader, None));
        let mut dependent = shader.clone();
        let mut next = dependent.instructions[0].clone();
        next.sources[0].register = Register {
            kind: RegisterKind::Temp,
            index: 0,
        };
        dependent.instructions.insert(1, next);
        assert!(coordinates(&dependent, None).is_none());
        for opcode in [Opcode::Dsx, Opcode::Dsy, Opcode::Texldd, Opcode::If] {
            let mut nonlocal = shader.clone();
            nonlocal.instructions[1].opcode = opcode;
            assert!(coordinates(&nonlocal, None).is_none());
        }
        let mut centroid = shader.clone();
        centroid.instructions[0]
            .destination
            .as_mut()
            .unwrap()
            .centroid = true;
        assert!(coordinates(&centroid, None).is_none());
        for controls in [1, 2] {
            let mut projected = shader.clone();
            projected.instructions[0].controls = controls;
            assert!(coordinates(&projected, None).is_none());
        }
    }
}
