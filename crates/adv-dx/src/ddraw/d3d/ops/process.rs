//! Scalar vertex transformation for the legacy CPU output interfaces.
use super::{Calls, INVALID, Object, read, render::interleave, write};
use crate::{
    argument::{Arguments, CallError},
    fixed::{self, Render, VertexLayout, tnl},
};
use adv_abi::types::D3DTLVERTEX;

use super::super::{desc::vertex, device, device_mut};

pub(super) fn argb(color: [f32; 4]) -> u32 {
    color
        .into_iter()
        .zip([16, 8, 0, 24])
        .fold(0, |word, (v, shift)| {
            word | ((v.clamp(0., 1.) * 255.).round() as u32) << shift
        })
}

pub(super) struct Processor<'a> {
    state: &'a fixed::State,
    lights: [Option<[f32; 26]>; 8],
    normals: [[f32; 3]; 3],
    lighting: bool,
}

impl<'a> Processor<'a> {
    pub(super) fn new(state: &'a fixed::State, lighting: bool) -> Result<Self, u32> {
        Ok(Self {
            state,
            lighting,
            lights: if lighting {
                tnl::active(state).map_err(|_| INVALID)?
            } else {
                [None; 8]
            },
            normals: if lighting {
                tnl::normal_matrix(state)
            } else {
                [[0.; 3]; 3]
            },
        })
    }

    pub(super) fn vertex(
        &self,
        src: &[u8],
        input: VertexLayout,
        dst: &mut [u8],
        output: VertexLayout,
        colors: bool,
    ) -> ([f32; 4], u32) {
        let word =
            |offset: u32| u32::from_le_bytes(src[offset as usize..][..4].try_into().unwrap());
        let float = |offset| f32::from_bits(word(offset));
        let mut point = [float(0), float(4), float(8), 1.];
        let mut clip_flags = 0;
        let mut source_color = input.color.map(word);
        let mut source_specular = input.specular.map(|(_, offset)| word(offset));
        if input.transformed {
            point[3] = float(12);
        } else {
            let projected = project(self.state, point);
            point = projected.screen;
            clip_flags = projected.flags;
            let eye = projected.eye;
            if let Some((_, offset)) = input.normal.filter(|_| self.lighting) {
                let normal = [float(offset), float(offset + 4), float(offset + 8)];
                let (diffuse, specular) = tnl::illuminate(
                    self.state,
                    &self.lights,
                    eye,
                    normal,
                    &self.normals,
                    source_color.map(tnl::color),
                    source_specular.map(tnl::color),
                );
                source_color = Some(argb(diffuse));
                source_specular = Some(argb(specular));
            }
            if self.state.render[&Render::FogEnable] != 0
                && self.state.render[&Render::VertexFog] != 0
                && self.state.render[&Render::TableFog] == 0
            {
                let fog = tnl::fog(self.state, false, point[2], eye, 1.);
                let specular = source_specular.unwrap_or(0);
                source_specular =
                    Some((specular & 0x00ff_ffff) | ((fog * 255.).round() as u32) << 24);
            }
        }
        let mut put = |offset: u32, value: u32| {
            dst[offset as usize..][..4].copy_from_slice(&value.to_le_bytes());
        };
        for (i, value) in point
            .into_iter()
            .enumerate()
            .take(if output.transformed { 4 } else { 3 })
        {
            put(i as u32 * 4, value.to_bits());
        }
        if colors {
            if let Some(offset) = output.color {
                put(offset, source_color.unwrap_or(u32::MAX));
            }
            if let Some((_, offset)) = output.specular {
                put(offset, source_specular.unwrap_or(0xff00_0000));
            }
        }
        for (source, destination) in input.texcoords.into_iter().zip(output.texcoords) {
            if let Some((_, dest, dimensions)) = destination {
                for n in 0..dimensions {
                    let value = source
                        .filter(|&(_, _, size)| n < size)
                        .map_or(0, |(_, offset, _)| word(offset + u32::from(n) * 4));
                    put(dest + u32::from(n) * 4, value);
                }
            }
        }
        (point, clip_flags)
    }
}

impl Calls<'_> {
    pub(super) fn process_vertices(
        &mut self,
        method: &str,
        args: &[u32],
    ) -> Result<u32, CallError> {
        let [dst, op, first, count, source, start, this, flags] =
            <[u32; 8]>::try_from(args).unwrap();
        let com = self.draw.com.unwrap();
        let Some(Object::VertexBuffer {
            desc, data, locked, ..
        }) = com.get::<Object>(self.objects, dst)
        else {
            return Ok(INVALID);
        };
        if *locked {
            return Ok(0x8876080e);
        }
        if first
            .checked_add(count)
            .is_none_or(|end| end > desc.dwNumVertices)
            || op & !0x40d != 0
            || op & 1 == 0
        {
            return Ok(INVALID);
        }
        let mut output = vertex::layout(desc.dwFVF).unwrap();
        if flags & 1 != 0 {
            output.texcoords = [None; 8];
        }
        if !output.transformed {
            return Ok(INVALID);
        }
        let dest = data + first * output.size;
        let (input, bytes) = if method == "ProcessVerticesStrided" {
            let Ok(layout) = vertex::layout(start) else {
                return Ok(INVALID);
            };
            let bytes = interleave(
                self.memory,
                read(self.memory, source)?,
                layout,
                count,
                (start & 0xe).saturating_sub(4) / 2,
            )?;
            (layout, bytes)
        } else {
            let Some(Object::VertexBuffer {
                desc, data, locked, ..
            }) = com.get::<Object>(self.objects, source)
            else {
                return Ok(INVALID);
            };
            if *locked {
                return Ok(0x8876080e);
            }
            if start
                .checked_add(count)
                .is_none_or(|end| end > desc.dwNumVertices)
            {
                return Ok(INVALID);
            }
            let layout = vertex::layout(desc.dwFVF).unwrap();
            (
                layout,
                self.memory
                    .arg_read(
                        data + start * layout.size,
                        count as usize * layout.size as usize,
                    )?
                    .into_owned(),
            )
        };
        let Some(dev) = device(self.draw, self.objects, this) else {
            return Ok(INVALID);
        };
        let state = &dev.state.as_ref().unwrap().fixed;
        let processor = match Processor::new(state, op & 0x400 != 0) {
            Ok(processor) => processor,
            Err(hr) => return Ok(hr),
        };
        let mut out = self
            .memory
            .arg_read(dest, count as usize * output.size as usize)?
            .into_owned();
        let mut union = 0;
        let mut intersection = 0x3f;
        let mut extents = [
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for (src, dst) in bytes
            .chunks_exact(input.size as usize)
            .zip(out.chunks_exact_mut(output.size as usize))
        {
            let (point, clip) =
                processor.vertex(src, input, dst, output, flags & 1 == 0 || op & 0x400 != 0);
            union |= clip;
            intersection &= clip;
            for (i, value) in point[..3].iter().copied().enumerate() {
                extents[i * 2] = extents[i * 2].min(value);
                extents[i * 2 + 1] = extents[i * 2 + 1].max(value);
            }
        }
        self.memory.arg_write(dest, &out)?;
        let dev = device_mut(self.draw, self.objects, this).unwrap();
        let clip_status = &mut dev.state.as_mut().unwrap().clip_status;
        if op & 4 != 0 {
            clip_status[1] = union | intersection << 12;
        }
        if op & 8 != 0 && count != 0 {
            clip_status[2..8].copy_from_slice(&extents.map(f32::to_bits));
        }
        dev.legacy.stats[4] += count;
        Ok(0)
    }
}

pub(super) fn tl_layout() -> VertexLayout {
    vertex::layout(0x1c4).unwrap()
}

pub(super) const TL_SIZE: usize = size_of::<D3DTLVERTEX>();

struct Projected {
    screen: [f32; 4],
    homogeneous: [f32; 4],
    eye: [f32; 4],
    flags: u32,
}

fn project(state: &fixed::State, point: [f32; 4]) -> Projected {
    let [world, view, projection] = state.transforms;
    let eye = tnl::row(tnl::row(point, &world), &view);
    let homogeneous @ [x, y, z, w] = tnl::row(eye, &projection);
    let flags = u32::from(x < -w)
        | u32::from(x > w) << 1
        | u32::from(y > w) << 2
        | u32::from(y < -w) << 3
        | u32::from(z < 0.) << 4
        | u32::from(z > w) << 5;
    let view = state.viewport;
    let rhw = 1. / w;
    let screen = [
        view.rect.x as f32 + (1. + x * rhw) * view.rect.w as f32 * 0.5,
        view.rect.y as f32 + (1. - y * rhw) * view.rect.h as f32 * 0.5,
        view.min_depth + z * rhw * (view.max_depth - view.min_depth),
        rhw,
    ];
    Projected {
        screen,
        homogeneous,
        eye,
        flags,
    }
}

impl Calls<'_> {
    pub(super) fn transform_vertices(&mut self, args: &[u32]) -> Result<u32, CallError> {
        use adv_abi::types::{D3DHVERTEX, D3DTRANSFORMDATA};
        let [viewport, count, address, flags, offscreen] = <[u32; 5]>::try_from(args).unwrap();
        let com = self.draw.com.unwrap();
        let Some(Object::Viewport {
            device: Some(owner),
            value,
            ..
        }) = com.get::<Object>(self.objects, viewport)
        else {
            return Ok(0x88760306);
        };
        let dev = device(self.draw, self.objects, *owner).unwrap();
        let mut state = dev.state.as_ref().unwrap().fixed.clone();
        state.viewport = fixed::Viewport {
            rect: adv_device::surface::Rect {
                x: value[1],
                y: value[2],
                w: value[3],
                h: value[4],
            },
            min_depth: f32::from_bits(value[9]),
            max_depth: f32::from_bits(value[10]),
        };
        let data: D3DTRANSFORMDATA = read(self.memory, address)?;
        if data.dwSize != size_of::<D3DTRANSFORMDATA>() as u32
            || !matches!(flags, 1 | 2)
            || data.dwInSize < 12
            || data.dwOutSize < 16
        {
            return Ok(INVALID);
        }
        let clipped = flags == 1;
        let mut intersection = if clipped { 0x3f } else { 0 };
        let mut union = 0;
        let payload = (data.dwInSize - 12).min(data.dwOutSize - 16);
        for n in 0..count {
            let input = data.lpIn.wrapping_add(n.wrapping_mul(data.dwInSize));
            let output = data.lpOut.wrapping_add(n.wrapping_mul(data.dwOutSize));
            let [x, y, z]: [f32; 3] = read(self.memory, input)?;
            let point = project(&state, [x, y, z, 1.]);
            if clipped {
                intersection &= point.flags;
                union |= point.flags;
                let [hx, hy, hz, _] = point.homogeneous;
                write(
                    self.memory,
                    data.lpHOut.wrapping_add(n.wrapping_mul(16)),
                    &D3DHVERTEX {
                        dwFlags: point.flags,
                        hx,
                        hy,
                        hz,
                    },
                )?;
            }
            if clipped && point.flags != 0 {
                write(self.memory, output, &point.homogeneous)?;
            } else {
                let bytes = self
                    .memory
                    .arg_read(input + 12, payload as usize)?
                    .into_owned();
                write(self.memory, output, &point.screen)?;
                self.memory.arg_write(output + 16, &bytes)?;
            }
        }
        self.memory.arg_put_u32(offscreen, intersection)?;
        if clipped {
            self.memory.arg_put_u32(address + 28, intersection)?;
            self.memory.arg_put_u32(address + 32, union)?;
        }
        Ok(0)
    }
}
