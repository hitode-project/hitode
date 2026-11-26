//! Capture conservative draw bounds and sampling mappings at D3D9 admission.
use super::*;
use adv_device::{
    gpu::{AttributeFormat, Image},
    surface::Rect,
};

pub(super) mod sampling;

pub(super) fn image(operation: &Operation) -> Option<&Image> {
    match operation {
        Operation::Draw(d) => Some(&d.target.image),
        Operation::Clear { target, .. } | Operation::Upload { target, .. } => Some(&target.image),
        Operation::Copy { destination, .. } => Some(&destination.image),
        _ => None,
    }
}
pub(super) fn full(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    }
}
pub(in crate::d3d9) fn bounds(env: &Env<'_>, device: &Device, operation: &Operation) -> Rect {
    match operation {
        Operation::Clear { rectangle, .. } | Operation::Upload { rectangle, .. } => *rectangle,
        Operation::Copy { region, .. } => *region,
        Operation::Draw(d) => draw_bounds(env, device, d).unwrap_or_else(|| full(&d.target.image)),
        _ => Rect::default(),
    }
}
fn draw_bounds(env: &Env<'_>, device: &Device, d: &fixed::Draw) -> Option<Rect> {
    use crate::shader::interpreter::{self, NoTextures};
    if d.programmable
        .as_ref()
        .is_some_and(|p| p.instance_count != 1 || p.streams.iter().any(|s| s.instance))
    {
        return None;
    }
    let mut positions = [[0.; 4]; 64];
    let count;
    let point = d.programmable.as_ref().filter(|p| p.parameters[2][3] != 0);
    let point_size = |value: f32| -> Option<f32> {
        let p = point?;
        let lo = f32::from_bits(p.parameters[12][0]);
        let hi = f32::from_bits(p.parameters[12][1]);
        (value.is_finite() && lo.is_finite() && hi.is_finite() && lo >= 0. && lo <= hi)
            .then(|| value.clamp(lo, hi))
    };
    let mut margin = if let Some(p) = point {
        1. + point_size(f32::from_bits(p.parameters[11][3]))? * 0.5
    } else {
        1.
    };
    if let Some(fixed) = &d.fixed_vertex {
        let p = d.programmable.as_ref()?;
        let length = p.indices.as_ref().map_or(fixed.count as usize, |i| i.len());
        if length > 64 {
            return None;
        }
        count = length;
        for (n, position) in positions[..count].iter_mut().enumerate() {
            let (clip, eye) = fixed_position(d, n)?;
            *position = clip;
            if let Some([a, b, c]) = fixed.point_scale {
                let [x, y, z] = eye;
                let distance = (x * x + y * y + z * z).sqrt();
                let size = d.state.viewport.height * f32::from_bits(p.parameters[11][3])
                    / (a + b * distance + c * distance * distance).sqrt();
                margin = margin.max(1. + point_size(size)? * 0.5);
            }
        }
    } else if !d.vertices.is_empty() {
        if d.vertices.len() > 64 {
            return None;
        }
        count = d.vertices.len();
        for (out, v) in positions.iter_mut().zip(d.vertices.iter()) {
            *out = v.position;
        }
    } else {
        let p = d.programmable.as_ref()?;
        if p.vertex_count > 64 {
            return None;
        }
        count = p.vertex_count as usize;
        if p.parameters[15][0] != 0 {
            for n in 0..p.vertex_count {
                let input = vertex_input(p, n)?;
                positions[n as usize] =
                    crate::fixed::tnl::pretransformed(input.v[0], d.state.viewport)?;
            }
        } else {
            let shader = shaders::bound(env, device.hot.borrow().shaders.vertex?)?;
            let translation = shader.translation.as_ref().ok()?;
            for n in 0..p.vertex_count {
                let input = vertex_input(p, n)?;
                let output =
                    interpreter::run(&shader.parsed, &input, &p.vertex_constants, &mut NoTextures)
                        .ok()?;
                if point.is_some() && translation.writes_point_size {
                    margin = margin.max(1. + point_size(output.point_size)? * 0.5);
                }
                let mut position = output.position;
                position[0] += position[3] / d.state.viewport.width;
                position[1] -= position[3] / d.state.viewport.height;
                positions[n as usize] = position;
            }
        }
    }
    let mut bounds = Bounds::default();
    for &p in &positions[..count] {
        if p.iter().any(|x| !x.is_finite()) || p[3] <= 0. {
            return None;
        }
        let screen = screen(p, d.state.viewport);
        if !screen.into_iter().all(f32::is_finite) {
            return None;
        }
        bounds.add(screen);
    }
    margin.is_finite().then(|| bounds.rectangle(d, margin))
}

pub(super) fn screen(p: [f32; 4], v: adv_device::gpu::Viewport) -> [f32; 2] {
    [
        v.x + (p[0] / p[3] + 1.) * v.width * 0.5,
        v.y + (1. - p[1] / p[3]) * v.height * 0.5,
    ]
}

pub(super) struct Bounds {
    lo: [f32; 2],
    hi: [f32; 2],
}
impl Default for Bounds {
    fn default() -> Self {
        Self {
            lo: [f32::INFINITY; 2],
            hi: [f32::NEG_INFINITY; 2],
        }
    }
}
impl Bounds {
    pub fn add(&mut self, screen: [f32; 2]) {
        for (n, v) in screen.into_iter().enumerate() {
            self.lo[n] = self.lo[n].min(v);
            self.hi[n] = self.hi[n].max(v);
        }
    }

    pub fn rectangle(self, d: &fixed::Draw, margin: f32) -> Rect {
        let v = d.state.viewport;
        let x = (self.lo[0] - margin).floor().max(v.x).max(0.) as u32;
        let y = (self.lo[1] - margin).floor().max(v.y).max(0.) as u32;
        let r = Rect {
            x,
            y,
            w: ((self.hi[0] + margin).ceil().min(v.x + v.width) as u32).saturating_sub(x),
            h: ((self.hi[1] + margin).ceil().min(v.y + v.height) as u32).saturating_sub(y),
        }
        .clipped(full(&d.target.image));
        d.state.scissor.map_or(r, |s| r.clipped(s))
    }
}

fn fixed_index(p: &adv_device::gpu::ProgrammableDraw, ordinal: usize) -> usize {
    if let Some(indices) = &p.indices {
        indices[ordinal] as usize
    } else if p.parameters[2][3] != 0 {
        ordinal * 6
    } else {
        ordinal
    }
}

fn fixed_position(d: &fixed::Draw, ordinal: usize) -> Option<([f32; 4], [f32; 3])> {
    let fixed = d.fixed_vertex.as_ref()?;
    let p = d.programmable.as_ref()?;
    let n = fixed_index(p, ordinal);
    let (stream, attribute) = p.streams.iter().find_map(|stream| {
        stream
            .attributes
            .iter()
            .find(|a| a.location == 0)
            .map(|a| (stream, a))
    })?;
    let mut bytes = [0; 16];
    stream.bytes.read_vertex(
        n,
        stream.stride as usize,
        attribute.offset as usize,
        &mut bytes[..attribute.format.size() as usize],
    )?;
    let raw = std::array::from_fn(|n| f32::from_le_bytes(bytes[n * 4..][..4].try_into().unwrap()));
    crate::fixed::tnl::clip(raw, fixed.transforms.as_ref(), d.state.viewport)
}

pub(in crate::d3d9) fn vertex_input(
    p: &adv_device::gpu::ProgrammableDraw,
    n: u32,
) -> Option<crate::shader::interpreter::Inputs> {
    stream_input(&p.streams, n)
}
pub(in crate::d3d9) fn stream_input(
    streams: &[adv_device::gpu::VertexStream],
    n: u32,
) -> Option<crate::shader::interpreter::Inputs> {
    let mut input = crate::shader::interpreter::Inputs::default();
    for stream in streams {
        for a in &stream.attributes {
            if a.format == AttributeFormat::Bgra8Unorm {
                let mut bytes = [0; 4];
                stream.bytes.read_vertex(
                    n as usize,
                    stream.stride as usize,
                    a.offset as usize,
                    &mut bytes,
                )?;
                input.v[a.location as usize] =
                    [bytes[2], bytes[1], bytes[0], bytes[3]].map(|v| f32::from(v) / 255.);
                continue;
            }
            if !matches!(
                a.format,
                AttributeFormat::Float1
                    | AttributeFormat::Float2
                    | AttributeFormat::Float3
                    | AttributeFormat::Float4
            ) {
                return None;
            }
            let mut v = [0., 0., 0., 1.];
            let mut attribute = [0; 16];
            let bytes = &mut attribute[..a.format.size() as usize];
            stream.bytes.read_vertex(
                n as usize,
                stream.stride as usize,
                a.offset as usize,
                bytes,
            )?;
            for (component, bytes) in bytes.as_chunks::<4>().0.iter().enumerate() {
                v[component] = f32::from_le_bytes(*bytes);
            }
            *input.v.get_mut(a.location as usize)? = v;
        }
    }
    Some(input)
}
