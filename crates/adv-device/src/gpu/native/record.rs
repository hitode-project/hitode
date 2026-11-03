//! Guest operations as command chunks. Validation lives here; the owner
//! assumes admitted commands and ring ranges are valid.

use super::super::{self as gpu, DrawState, Error, Filter, ProgrammableDraw, Rect, Vertex};
use super::pipelines::slot;
use super::resources::Storage;
use super::stream::{Binding, Bytes, Command, Draw, Producer};
use super::{Depth, Gpu, Image, ImageId, Target, backing};
use crate::frame::Damage;
use adv_gpu::resource::{BufferDesc, BufferUsage, Memory};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

const MAX_UPLOAD: usize = 16 << 20;
const MAX_READBACKS: usize = 8;
const CONSTANT_SIZE: usize = std::mem::size_of::<gpu::ConstantBlock>();

pub(super) struct Recording {
    target: Option<(ImageId, Option<Depth>)>,
    white: Image,
    samplers: HashMap<gpu::Sampler, adv_gpu::Sampler>,
    readbacks: Arc<AtomicUsize>,
    pub readback_limit: usize,
}
impl Recording {
    pub fn new(device: &adv_gpu::Device, stream: &mut Producer) -> Result<Self, Error> {
        let image = device
            .create_image(&backing::image_desc(
                (1, 1),
                adv_gpu::Format::Bgra8Unorm,
                adv_gpu::resource::Usage::SAMPLED | adv_gpu::resource::Usage::COPY_DST,
            ))
            .map_err(gpu_error)?;
        let white = Image {
            storage: Storage::Fixed {
                view: image.view().clone(),
                image,
                level: None,
            },
            alpha: gpu::AlphaMode::Stored,
        };
        let bytes = stream.bytes(4, 4, |bytes| bytes.fill(255));
        stream.push(Command::Upload {
            bytes,
            pitch: 4,
            image: white.clone(),
            mip: 0,
            rect: adv_gpu::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        });
        Ok(Self {
            target: None,
            white,
            samplers: HashMap::new(),
            readbacks: Arc::new(AtomicUsize::new(0)),
            readback_limit: MAX_READBACKS,
        })
    }
}

struct Completion {
    result: Mutex<Option<Result<Vec<u8>, Error>>>,
    ready: Condvar,
    cancelled: AtomicBool,
    finished: AtomicBool,
}
/// An explicitly requested image transfer. Completion owns the mapped buffer
/// until its submission finishes, independently of the guest's handle.
pub struct Readback {
    pub width: u32,
    pub height: u32,
    completion: Arc<Completion>,
    job_taken: bool,
}
impl Readback {
    pub fn cancel(&self) {
        self.completion.cancelled.store(true, Ordering::Release);
    }
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, Error>> {
        self.completion.result.lock().unwrap().take()
    }
    pub fn take_completion_job(&mut self) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        if std::mem::replace(&mut self.job_taken, true) {
            return None;
        }
        let completion = self.completion.clone();
        Some(Box::new(move || {
            let mut result = completion.result.lock().unwrap();
            while !completion.finished.load(Ordering::Acquire) {
                result = completion.ready.wait(result).unwrap();
            }
        }))
    }
}

impl Gpu {
    pub(super) fn attachment_dimensions(&self, width: u32, height: u32) -> Result<(), Error> {
        let limit = self.device.caps().limits.max_image_2d;
        if width == 0 || height == 0 || width.max(height) > limit {
            return Err(invalid("invalid attachment dimensions"));
        }
        self.check()
    }
    fn select(&mut self, target: &Target) {
        let same = self
            .recording
            .target
            .as_ref()
            .is_some_and(|(image, depth)| {
                *image == target.image.id() && same_depth(depth.as_ref(), target.depth.as_ref())
            });
        if !same {
            self.stream.push(Command::Target {
                color: target.image.clone(),
                depth: target.depth.clone(),
            });
            self.recording.target = Some((target.image.id(), target.depth.clone()));
        }
    }
    pub fn clear(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
    ) -> Result<(), Error> {
        self.clear_with_stencil(target, color, depth, None)
    }
    pub fn clear_region(
        &mut self,
        target: &Target,
        rectangle: Rect,
        mut color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        rectangle_in(&target.image, rectangle)?;
        if color.is_none() && depth.is_none() && stencil.is_none()
            || color.is_some_and(|c| c.iter().any(|v| !v.is_finite() || !(0. ..=1.).contains(v)))
            || depth.is_some_and(|d| !d.is_finite() || !(0. ..=1.).contains(&d))
            || depth.is_some() && target.depth.is_none()
            || stencil.is_some()
                && target
                    .depth
                    .as_ref()
                    .is_none_or(|d| !d.format().has_stencil())
        {
            return Err(invalid("invalid clear value or attachment"));
        }
        let area = self.clip.map_or(rectangle, |clip| rectangle.clipped(clip));
        if area.empty() {
            return Ok(());
        }
        if target.image.alpha_mode() == gpu::AlphaMode::Opaque
            && let Some(color) = &mut color
        {
            color[3] = 1.;
        }
        self.select(target);
        self.stream.push(Command::Clear {
            area: native_rect(area),
            color,
            depth,
            stencil: stencil.map(|s| s & 255),
        });
        self.check()
    }
    pub fn clear_with_stencil(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        self.clear_region(target, whole(&target.image), color, depth, stencil)
    }
    pub fn draw(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        image: Option<&Image>,
        mut state: DrawState,
    ) -> Result<(), Error> {
        validate_state(target, &state)?;
        validate_count(vertices.len(), state.topology)?;
        if std::mem::size_of_val(vertices) > MAX_UPLOAD
            || vertices.iter().any(|v| {
                v.position
                    .iter()
                    .chain(&v.color)
                    .chain(&v.uv)
                    .any(|v| !v.is_finite())
            })
        {
            return Err(invalid("invalid vertex data or upload size"));
        }
        if image.is_some_and(|image| image.shares_storage(&target.image)) {
            return Err(invalid("render target cannot sample itself"));
        }
        normalize(target, &mut state);
        let inclusive = gpu::line::prepare(vertices, image, &state)?;
        let area = draw_area(target, &state, self.clip);
        if area.w == 0 || area.h == 0 {
            return Ok(());
        }
        self.select(target);
        let colors = [color_format(&target.image)];
        let depth = target
            .depth
            .as_ref()
            .map(|d| backing::depth_format(d.format()));
        let pipeline = self.pipelines.fixed(&self.device, &state, &colors, depth)?;
        let endpoint = if inclusive.is_some() {
            let mut point = state;
            point.topology = gpu::Topology::Points;
            Some(self.pipelines.fixed(&self.device, &point, &colors, depth)?)
        } else {
            None
        };
        match inclusive {
            None => self.fixed_draw(vertices, image, &state, pipeline, area),
            Some(gpu::line::Inclusive::Strip { terminal }) => {
                self.fixed_draw(vertices, image, &state, pipeline, area)?;
                if terminal {
                    let mut state = state;
                    state.topology = gpu::Topology::Points;
                    self.fixed_draw(
                        &vertices[vertices.len() - 1..],
                        image,
                        &state,
                        endpoint.unwrap(),
                        area,
                    )?;
                }
                Ok(())
            }
            Some(gpu::line::Inclusive::List) => {
                for pair in vertices.as_chunks::<2>().0 {
                    self.fixed_draw(pair, image, &state, pipeline.clone(), area)?;
                    if !gpu::line::degenerate(&pair[0], &pair[1], state.viewport) {
                        let mut point = state;
                        point.topology = gpu::Topology::Points;
                        self.fixed_draw(
                            &pair[1..],
                            image,
                            &point,
                            endpoint.as_ref().unwrap().clone(),
                            area,
                        )?;
                    }
                }
                Ok(())
            }
        }
    }

    fn fixed_draw(
        &mut self,
        vertices: &[Vertex],
        image: Option<&Image>,
        state: &DrawState,
        pipeline: adv_gpu::RenderPipeline,
        area: adv_gpu::Rect,
    ) -> Result<(), Error> {
        let texture = image.unwrap_or(&self.recording.white).clone();
        let sampler = self.sampler(state.sampler)?;
        let parameters = fixed_parameters(state, image);
        let alignment = self.data_alignment();
        let vertex_offset = 32usize.next_multiple_of(alignment as usize);
        let len = vertex_offset + std::mem::size_of_val(vertices);
        let data = self.stream.bytes(len as u32, alignment, |bytes| {
            bytes[..32].copy_from_slice(bytemuck::cast_slice(&parameters));
            bytes[vertex_offset..].copy_from_slice(bytemuck::cast_slice(vertices));
            #[cfg(target_vendor = "apple")]
            if matches!(
                state.topology,
                gpu::Topology::Lines | gpu::Topology::LineStrip
            ) {
                for (bytes, vertex) in bytes[vertex_offset..]
                    .as_chunks_mut::<40>()
                    .0
                    .iter_mut()
                    .zip(vertices)
                {
                    let y = vertex.position[1] + vertex.position[3] / (64. * state.viewport.height);
                    bytes[4..8].copy_from_slice(&y.to_ne_bytes());
                }
            }
        });
        let mut draw = summary(state, pipeline, area, 0..vertices.len() as u32);
        draw.opaque = opaque_quad(vertices, state, area);
        self.stream.draw(
            draw,
            [
                (slot::FRAGMENT_UNIFORMS, Binding::Uniform(part(data, 0, 32))),
                (
                    slot::STREAMS,
                    Binding::Storage(part(
                        data,
                        vertex_offset as u32,
                        (len - vertex_offset) as u32,
                    )),
                ),
                (slot::TEXTURES, Binding::Sampled(texture)),
                (slot::SAMPLERS, Binding::Sampler(sampler)),
            ],
        );
        self.stats.draws += 1;
        self.stats.vertices += vertices.len() as u64;
        self.check()
    }

    pub fn draw_programmable(
        &mut self,
        target: &Target,
        draw: &ProgrammableDraw,
        state: &DrawState,
    ) -> Result<(), Error> {
        validate_state(target, state)?;
        let count = draw
            .indices
            .as_ref()
            .map_or(draw.vertex_count as usize, |i| i.len());
        validate_count(count, state.topology)?;
        if draw.instance_count == 0 || count > 0x300000 {
            return Err(invalid("invalid programmable primitive count"));
        }
        if draw.streams.len() > 16
            || draw.textures.len() != draw.program.textures.len()
            || draw
                .program
                .textures
                .iter()
                .filter(|&&vertex| vertex)
                .count()
                > 4
            || draw
                .program
                .textures
                .iter()
                .filter(|&&vertex| !vertex)
                .count()
                > 16
        {
            return Err(invalid("invalid programmable bindings"));
        }
        if draw.program.source().len() > gpu::Program::MAX_SOURCE_LEN {
            return Err(invalid("shader source exceeds limit"));
        }
        let last = draw
            .indices
            .as_ref()
            .and_then(|i| i.maximum())
            .unwrap_or(draw.vertex_count.saturating_sub(1));
        if last >= draw.vertex_count {
            return Err(invalid("index outside vertex span"));
        }
        let mut locations = 0u16;
        let mut total = 0usize;
        for stream in &draw.streams {
            if stream.stride > 2048
                || !stream.stride.is_multiple_of(4)
                || stream.attributes.is_empty()
            {
                return Err(invalid("invalid vertex stride"));
            }
            for a in &stream.attributes {
                let end = a
                    .offset
                    .checked_add(a.format.size())
                    .ok_or_else(|| invalid("attribute offset overflow"))?;
                if a.location >= 16
                    || locations & (1 << a.location) != 0
                    || !a.offset.is_multiple_of(4)
                    || stream.stride != 0 && end > stream.stride
                    || u64::from(if stream.instance {
                        draw.instance_count - 1
                    } else {
                        last
                    }) * u64::from(stream.stride)
                        + u64::from(end)
                        > stream.bytes.len() as u64
                {
                    return Err(invalid("attribute outside vertex span"));
                }
                locations |= 1 << a.location;
            }
            total += stream.bytes.len().next_multiple_of(4);
        }
        if total > MAX_UPLOAD
            || draw
                .indices
                .as_ref()
                .is_some_and(|i| i.len() > MAX_UPLOAD / 4)
        {
            return Err(invalid("programmable upload exceeds limit"));
        }
        for binding in &draw.textures {
            if binding
                .image
                .as_ref()
                .is_some_and(|image| image.shares_storage(&target.image))
            {
                return Err(invalid("render target cannot sample itself"));
            }
        }
        let mut state = *state;
        normalize(target, &mut state);
        let area = draw_area(target, &state, self.clip);
        if area.w == 0 || area.h == 0 {
            return Ok(());
        }
        self.select(target);
        let pipeline = self.pipelines.programmable(
            &self.device,
            draw,
            &state,
            &[color_format(&target.image)],
            target
                .depth
                .as_ref()
                .map(|d| backing::depth_format(d.format())),
        )?;
        let alignment = self.data_alignment() as usize;
        let parameters_offset = CONSTANT_SIZE;
        let pixel_offset = (CONSTANT_SIZE + 640).next_multiple_of(alignment);
        let mut cursor = (pixel_offset + CONSTANT_SIZE).next_multiple_of(alignment);
        let mut stream_ranges = arrayvec::ArrayVec::<(u32, u32), 16>::new();
        for stream in &draw.streams {
            stream_ranges.push((cursor as u32, stream.bytes.len().next_multiple_of(4) as u32));
            cursor = (cursor + stream.bytes.len().next_multiple_of(4)).next_multiple_of(alignment);
        }
        let index_offset = cursor;
        cursor += draw.indices.as_ref().map_or(0, |i| i.len() * 4);
        let data = self.stream.bytes(cursor as u32, alignment as u32, |bytes| {
            bytes.fill(0);
            for (stage, constants) in [&draw.vertex_constants, &draw.pixel_constants]
                .into_iter()
                .enumerate()
            {
                let base = if stage == 0 { 0 } else { pixel_offset };
                for register in draw.program.constants[stage]
                    .ranges()
                    .flat_map(|range| range.start / 16..range.end / 16)
                {
                    bytes[base + register * 16..base + (register + 1) * 16]
                        .copy_from_slice(bytemuck::bytes_of(&constants.register(register)));
                }
            }
            bytes[parameters_offset..parameters_offset + 640]
                .copy_from_slice(bytemuck::bytes_of(&draw.parameters));
            for (stream, &(offset, _)) in draw.streams.iter().zip(&stream_ranges) {
                // Copy recipes directly, avoiding their deferred Vec payload.
                let width = if stream.stride == 0 {
                    stream
                        .attributes
                        .iter()
                        .map(|a| a.offset + a.format.size())
                        .max()
                        .unwrap() as usize
                } else {
                    stream.stride as usize
                };
                for (n, out) in bytes[offset as usize..offset as usize + stream.bytes.len()]
                    .chunks_mut(width)
                    .enumerate()
                {
                    stream
                        .bytes
                        .read_vertex(n, stream.stride as usize, 0, out)
                        .expect("validated vertex range");
                }
            }
            if let Some(indices) = &draw.indices {
                bytes[index_offset..].copy_from_slice(bytemuck::cast_slice(indices.as_slice()));
            }
        });
        let mut bindings = arrayvec::ArrayVec::<(u32, Binding), 58>::new();
        bindings.push((
            slot::VERTEX_UNIFORMS,
            Binding::Uniform(part(data, 0, (CONSTANT_SIZE + 640) as u32)),
        ));
        bindings.push((
            slot::FRAGMENT_UNIFORMS,
            Binding::Uniform(part(data, pixel_offset as u32, CONSTANT_SIZE as u32)),
        ));
        for (i, &(offset, len)) in stream_ranges.iter().enumerate() {
            bindings.push((
                slot::STREAMS + i as u32,
                Binding::Storage(part(data, offset, len)),
            ));
        }
        let mut pixel = 0;
        let mut vertex = 16;
        for (texture, &vertex_stage) in draw.textures.iter().zip(&draw.program.textures) {
            let ordinal = if vertex_stage {
                let n = vertex;
                vertex += 1;
                n
            } else {
                let n = pixel;
                pixel += 1;
                n
            };
            let image = texture
                .image
                .as_ref()
                .unwrap_or(&self.recording.white)
                .clone();
            let sampler = self.sampler(texture.sampler)?;
            bindings.push((slot::TEXTURES + ordinal, Binding::Sampled(image)));
            bindings.push((slot::SAMPLERS + ordinal, Binding::Sampler(sampler)));
        }
        let mut command = summary(&state, pipeline, area, 0..count as u32);
        command.instances = 0..draw.instance_count;
        command.uses_depth |= draw.program.writes_depth;
        command.writes_depth |= draw.program.writes_depth;
        command.index = draw.indices.as_ref().map(|i| {
            (
                Binding::Storage(part(data, index_offset as u32, (i.len() * 4) as u32)),
                adv_gpu::encode::IndexFormat::U32,
            )
        });
        self.stream.draw(command, bindings);
        self.stats.draws += 1;
        self.stats.vertices += count as u64;
        self.check()
    }

    pub fn blit(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
    ) -> Result<(), Error> {
        rectangle_in(source, rectangle)?;
        rectangle_in(destination, region)?;
        if source.shares_storage(destination) {
            return Err(invalid("image cannot blit onto itself"));
        }
        if source.alpha_mode() == gpu::AlphaMode::Opaque
            && destination.alpha_mode() == gpu::AlphaMode::Stored
        {
            return Err(Error::Unavailable(
                "opaque-to-stored color conversion is unavailable".into(),
            ));
        }
        if (rectangle.w, rectangle.h) == (region.w, region.h) {
            return if color_format(source) == color_format(destination) {
                self.copy_validated(source, rectangle, destination, region)
            } else {
                self.blit_validated(source, rectangle, destination, region, Filter::Nearest)
            };
        }
        if rectangle != whole(source) || filter != Filter::Linear {
            return Err(Error::Unavailable(
                "scaled blits require full-source LINEAR filtering".into(),
            ));
        }
        self.blit_validated(source, rectangle, destination, region, filter)
    }
    fn blit_validated(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        region: Rect,
        filter: Filter,
    ) -> Result<(), Error> {
        let coordinates = |src: u32, dst: u32| {
            if dst <= src {
                (0., 1.)
            } else if src == 1 {
                (0.5, 0.5)
            } else {
                (
                    0.,
                    dst as f32 * (src - 1) as f32 / (src as f32 * (dst - 1) as f32),
                )
            }
        };
        let (u0, u1) = if rectangle.w == region.w {
            (
                rectangle.x as f32 / source.width() as f32,
                (rectangle.x + rectangle.w) as f32 / source.width() as f32,
            )
        } else {
            coordinates(rectangle.w, region.w)
        };
        let (v0, v1) = if rectangle.h == region.h {
            (
                rectangle.y as f32 / source.height() as f32,
                (rectangle.y + rectangle.h) as f32 / source.height() as f32,
            )
        } else {
            coordinates(rectangle.h, region.h)
        };
        let x0 = region.x as f32 * 2. / destination.width() as f32 - 1.;
        let x1 = (region.x + region.w) as f32 * 2. / destination.width() as f32 - 1.;
        let y0 = 1. - region.y as f32 * 2. / destination.height() as f32;
        let y1 = 1. - (region.y + region.h) as f32 * 2. / destination.height() as f32;
        let v = |x, y, u, v| Vertex {
            position: [x, y, 0., 1.],
            color: [1.; 4],
            uv: [u, v],
        };
        let vertices = [
            v(x0, y0, u0, v0),
            v(x1, y0, u1, v0),
            v(x1, y1, u1, v1),
            v(x0, y0, u0, v0),
            v(x1, y1, u1, v1),
            v(x0, y1, u0, v1),
        ];
        let target = Target {
            image: destination.clone(),
            depth: None,
        };
        let mut state = DrawState::new(destination.width(), destination.height());
        state.scissor = Some(region);
        state.sampler.min = filter;
        state.sampler.mag = filter;
        state.texture_op = gpu::TextureOp::Texture;
        state.alpha_texture_op = gpu::TextureOp::Texture;
        let area = draw_area(&target, &state, self.clip);
        if area.w == 0 || area.h == 0 {
            return Ok(());
        }
        let pipeline = self
            .pipelines
            .blit(&self.device, filter, color_format(destination))?;
        self.select(&target);
        self.fixed_draw(&vertices, Some(source), &state, pipeline, area)
    }
    pub fn copy(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
    ) -> Result<(), Error> {
        rectangle_in(source, rectangle)?;
        let region = Rect {
            x,
            y,
            w: rectangle.w,
            h: rectangle.h,
        };
        rectangle_in(destination, region)?;
        if source.id() == destination.id() {
            return Err(invalid("image cannot copy onto itself"));
        }
        if color_format(source) != color_format(destination) {
            if !matches!(
                (color_format(source), color_format(destination)),
                (adv_gpu::Format::Rgba8Unorm, adv_gpu::Format::Bgra8Unorm)
                    | (adv_gpu::Format::Bgra8Unorm, adv_gpu::Format::Rgba8Unorm)
            ) {
                return Err(invalid("copy formats differ"));
            }
            return self.blit_validated(source, rectangle, destination, region, Filter::Nearest);
        }
        self.copy_validated(source, rectangle, destination, region)
    }
    fn copy_validated(
        &mut self,
        source: &Image,
        mut rectangle: Rect,
        destination: &Image,
        mut region: Rect,
    ) -> Result<(), Error> {
        if let Some(clip) = self.clip {
            let clipped = region.clipped(clip);
            if clipped.empty() {
                return Ok(());
            }
            rectangle.x += clipped.x - region.x;
            rectangle.y += clipped.y - region.y;
            rectangle.w = clipped.w;
            rectangle.h = clipped.h;
            region = clipped;
        }
        self.stream.push(Command::Copy {
            source: source.clone(),
            rect: native_rect(rectangle),
            destination: destination.clone(),
            at: (region.x, region.y),
        });
        self.stats.image_copy_bytes += u64::from(rectangle.w) * u64::from(rectangle.h) * 4;
        self.check()
    }
    pub fn present(&mut self, image: &Image, damage: Damage, serial: u64) -> Result<(), Error> {
        if let Damage::Rects(rects) = &damage {
            for &r in rects {
                rectangle_in(
                    image,
                    Rect {
                        x: r.x,
                        y: r.y,
                        w: r.w,
                        h: r.h,
                    },
                )?;
            }
        }
        self.stream.push(Command::Present {
            image: image.clone(),
            damage,
            serial,
        });
        self.check()
    }
    /// D3D9 D3DPRESENTFLAG_DISCARD_DEPTHSTENCIL ends the depth contents.
    pub fn discard_depth(&mut self, depth: &Depth) {
        self.stream.push(Command::DiscardDepth(depth.clone()));
    }
    pub fn flush(&mut self) -> Result<(), Error> {
        self.stream.push(Command::Submit);
        self.check()
    }
    pub fn readback(
        &mut self,
        image: &Image,
        wake: Option<Arc<adv_sync::WakeEvent>>,
    ) -> Result<Readback, Error> {
        self.check()?;
        let format = color_format(image);
        if !matches!(
            format,
            adv_gpu::Format::Bgra8Unorm | adv_gpu::Format::Rgba8Unorm
        ) {
            return Err(Error::Unavailable("readback requires 32-bit color".into()));
        }
        self.recording
            .readbacks
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < self.recording.readback_limit).then_some(n + 1)
            })
            .map_err(|_| Error::Busy)?;
        let reservation = Reservation(self.recording.readbacks.clone());
        let buffer = self
            .device
            .create_buffer(&BufferDesc {
                size: u64::from(image.width()) * u64::from(image.height()) * 4,
                usage: BufferUsage::COPY_DST,
                memory: Memory::Readback,
                label: "guest readback",
            })
            .map_err(gpu_error)?;
        let completion = Arc::new(Completion {
            result: Mutex::new(None),
            ready: Condvar::new(),
            cancelled: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let mut transfer = Transfer {
            completion: completion.clone(),
            reservation: Some(reservation),
            wake,
            device: self.device.clone(),
        };
        let device = self.device.clone();
        self.stream.push(Command::Readback {
            image: image.clone(),
            rect: native_rect(whole(image)),
            into: buffer.clone(),
            done: Box::new(move || {
                let result = if let Some(lost) = device.lost() {
                    Err(Error::Device(lost))
                } else if transfer.completion.cancelled.load(Ordering::Acquire) {
                    Err(Error::Device("GPU readback cancelled".into()))
                } else {
                    // Driver-owned persistent readback mapping, accessed only
                    // after this copy's completion token signals.
                    let mapping = buffer.mapped().expect("readback mapping");
                    let mut pixels = unsafe { mapping.as_ref() }.to_vec();
                    if format == adv_gpu::Format::Rgba8Unorm {
                        for pixel in pixels.as_chunks_mut::<4>().0 {
                            pixel.swap(0, 2);
                        }
                    }
                    Ok(pixels)
                };
                transfer.finish(result);
            }),
        });
        self.stats.readbacks += 1;
        Ok(Readback {
            width: image.width(),
            height: image.height(),
            completion,
            job_taken: false,
        })
    }
    pub fn set_readback_limit(&mut self, limit: usize) -> Result<(), Error> {
        if limit == 0 || limit > MAX_READBACKS {
            return Err(invalid("invalid readback budget"));
        }
        if self.recording.readbacks.load(Ordering::Acquire) > limit {
            return Err(Error::Busy);
        }
        self.recording.readback_limit = limit;
        Ok(())
    }
    pub fn poll(&self) -> Result<(), Error> {
        self.check()
    }

    fn data_alignment(&self) -> u32 {
        self.device
            .caps()
            .limits
            .uniform_offset_alignment
            .max(self.device.caps().limits.storage_offset_alignment)
            .max(4)
    }
    fn sampler(&mut self, mut spec: gpu::Sampler) -> Result<adv_gpu::Sampler, Error> {
        // Border sampling is shader code, as in the reference renderer.
        if spec.u == gpu::Address::Border {
            spec.u = gpu::Address::Clamp;
        }
        if spec.v == gpu::Address::Border {
            spec.v = gpu::Address::Clamp;
        }
        if let Some(sampler) = self.recording.samplers.get(&spec) {
            return Ok(sampler.clone());
        }
        let filter = |f| {
            if f == Filter::Linear {
                adv_gpu::resource::Filter::Linear
            } else {
                adv_gpu::resource::Filter::Nearest
            }
        };
        let address = |a| match a {
            gpu::Address::Clamp | gpu::Address::Border => adv_gpu::resource::Address::Clamp,
            gpu::Address::Repeat => adv_gpu::resource::Address::Repeat,
            gpu::Address::Mirror => adv_gpu::resource::Address::Mirror,
        };
        let sampler = self
            .device
            .sampler(&adv_gpu::resource::SamplerDesc {
                min: filter(spec.min),
                mag: filter(spec.mag),
                mip: filter(spec.mip.unwrap_or(Filter::Nearest)),
                address: [
                    address(spec.u),
                    address(spec.v),
                    adv_gpu::resource::Address::Clamp,
                ],
                border: adv_gpu::resource::BorderColor::TransparentBlack,
                lod_bias: 0,
                lod_min: (spec.first_level.min(255) * 256) as u16,
                lod_max: if spec.mip.is_some() {
                    u16::MAX
                } else {
                    (spec.first_level.min(255) * 256) as u16
                },
                anisotropy: 1,
                compare: None,
            })
            .map_err(gpu_error)?;
        self.recording.samplers.insert(spec, sampler.clone());
        Ok(sampler)
    }
}
struct Reservation(Arc<AtomicUsize>);
impl Drop for Reservation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Transfer {
    completion: Arc<Completion>,
    reservation: Option<Reservation>,
    wake: Option<Arc<adv_sync::WakeEvent>>,
    device: adv_gpu::Device,
}
impl Transfer {
    fn finish(&mut self, result: Result<Vec<u8>, Error>) {
        let mut output = self.completion.result.lock().unwrap();
        *output = Some(result);
        self.reservation.take();
        if let Some(wake) = &self.wake {
            wake.signal();
        }
        self.completion.finished.store(true, Ordering::Release);
        drop(output);
        self.completion.ready.notify_all();
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        if !self.completion.finished.load(Ordering::Acquire) {
            self.finish(Err(Error::Device(self.device.lost().unwrap_or_else(
                || "GPU owner stopped before readback completed".into(),
            ))));
        }
    }
}

pub(super) fn gpu_error(error: adv_gpu::Error) -> Error {
    match error {
        adv_gpu::Error::Unavailable(m) => Error::Unavailable(m),
        adv_gpu::Error::Invalid(m) => Error::Invalid(m),
        adv_gpu::Error::Busy => Error::Busy,
        error => Error::Device(error.to_string()),
    }
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn whole(image: &Image) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: image.width(),
        h: image.height(),
    }
}
fn native_rect(r: Rect) -> adv_gpu::Rect {
    adv_gpu::Rect {
        x: r.x,
        y: r.y,
        w: r.w,
        h: r.h,
    }
}
fn rectangle_in(image: &Image, r: Rect) -> Result<(), Error> {
    if r.empty()
        || r.x.checked_add(r.w).is_none_or(|x| x > image.width())
        || r.y.checked_add(r.h).is_none_or(|y| y > image.height())
    {
        return Err(invalid("rectangle is outside image"));
    }
    Ok(())
}
fn same_depth(a: Option<&Depth>, b: Option<&Depth>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(&a.0, &b.0),
        _ => false,
    }
}
fn color_format(image: &Image) -> adv_gpu::Format {
    match &image.storage {
        Storage::Fixed { image, .. } => image.desc().format,
        Storage::Swap(_) => backing::swap_format(),
    }
}
fn validate_count(count: usize, topology: gpu::Topology) -> Result<(), Error> {
    let valid = match topology {
        gpu::Topology::Points => count > 0,
        gpu::Topology::Lines => count > 0 && count.is_multiple_of(2),
        gpu::Topology::LineStrip => count >= 2,
        gpu::Topology::Triangles => count > 0 && count.is_multiple_of(3),
        gpu::Topology::TriangleStrip => count >= 3,
    };
    if !valid {
        return Err(invalid("invalid primitive count"));
    }
    Ok(())
}
fn validate_state(target: &Target, state: &DrawState) -> Result<(), Error> {
    let v = state.viewport;
    if [
        v.x,
        v.y,
        v.width,
        v.height,
        v.minimum_depth,
        v.maximum_depth,
    ]
    .iter()
    .any(|v| !v.is_finite())
        || v.x < 0.
        || v.y < 0.
        || v.width <= 0.
        || v.height <= 0.
        || v.x + v.width > target.image.width() as f32
        || v.y + v.height > target.image.height() as f32
        || v.minimum_depth < 0.
        || v.maximum_depth > 1.
        || v.minimum_depth > v.maximum_depth
        || state.color_write & !15 != 0
        || !state.alpha_reference.is_finite()
        || !(0. ..=1.).contains(&state.alpha_reference)
        || (state.depth.is_some() || state.depth_write) && target.depth.is_none()
        || state.stencil.is_some()
            && target
                .depth
                .as_ref()
                .is_none_or(|d| !d.format().has_stencil())
    {
        return Err(invalid("invalid drawing state or viewport"));
    }
    if let Some(r) = state.scissor {
        rectangle_in(&target.image, r)?;
    }
    Ok(())
}
fn normalize(target: &Target, state: &mut DrawState) {
    if let Some(s) = &mut state.stencil {
        s.read_mask &= 255;
        s.write_mask &= 255;
    }
    if target.image.alpha_mode() == gpu::AlphaMode::Opaque
        && let Some(blend) = &mut state.blend
    {
        let factor = |f| match f {
            gpu::Factor::DestinationAlpha => gpu::Factor::One,
            gpu::Factor::OneMinusDestinationAlpha | gpu::Factor::SourceAlphaSaturated => {
                gpu::Factor::Zero
            }
            f => f,
        };
        blend.source = factor(blend.source);
        blend.destination = factor(blend.destination);
    }
}
fn draw_area(target: &Target, state: &DrawState, clip: Option<Rect>) -> adv_gpu::Rect {
    let v = state.viewport;
    let viewport = Rect {
        x: v.x.floor() as u32,
        y: v.y.floor() as u32,
        w: (v.x + v.width).ceil() as u32 - v.x.floor() as u32,
        h: (v.y + v.height).ceil() as u32 - v.y.floor() as u32,
    };
    let area = state
        .scissor
        .unwrap_or_else(|| whole(&target.image))
        .clipped(viewport);
    native_rect(clip.map_or(area, |clip| area.clipped(clip)))
}
fn summary(
    state: &DrawState,
    pipeline: adv_gpu::RenderPipeline,
    area: adv_gpu::Rect,
    vertices: std::ops::Range<u32>,
) -> Draw {
    let v = state.viewport;
    let stencil_writes = state.stencil.is_some_and(|s| {
        s.write_mask != 0
            && [s.front, s.back].iter().any(|s| {
                [s.fail, s.depth_fail, s.pass]
                    .iter()
                    .any(|op| *op != gpu::StencilOp::Keep)
            })
    });
    let blend = state.blend_constant.unwrap_or(0);
    Draw {
        pipeline,
        bindings: 0..0,
        index: None,
        vertices,
        instances: 0..1,
        base_vertex: 0,
        viewport: [
            v.x,
            v.y,
            v.width,
            v.height,
            v.minimum_depth,
            v.maximum_depth,
        ],
        scissor: area,
        stencil_reference: state.stencil_reference & 255,
        blend_constant: [0, 8, 16, 24].map(|shift| ((blend >> shift) & 255) as f32 / 255.),
        area,
        writes_color: state.color_write != 0,
        uses_depth: state.depth.is_some() || state.depth_write || state.stencil.is_some(),
        writes_depth: state.depth_write || stencil_writes,
        opaque: None,
    }
}
fn part(bytes: Bytes, offset: u32, len: u32) -> Bytes {
    Bytes {
        span: bytes.span,
        offset: bytes.offset + offset,
        len,
    }
}
fn fixed_parameters(state: &DrawState, image: Option<&Image>) -> [u32; 8] {
    let op = |op| match op {
        gpu::TextureOp::Color => 0,
        gpu::TextureOp::Texture => 1,
        gpu::TextureOp::Multiply => 2,
        gpu::TextureOp::Add => 3,
        gpu::TextureOp::Subtract => 4,
    };
    let compare = match state.alpha_test {
        gpu::Compare::Never => 0,
        gpu::Compare::Less => 1,
        gpu::Compare::Equal => 2,
        gpu::Compare::LessEqual => 3,
        gpu::Compare::Greater => 4,
        gpu::Compare::NotEqual => 5,
        gpu::Compare::GreaterEqual => 6,
        gpu::Compare::Always => 7,
    };
    let mut sampler = state.sampler;
    sampler.first_level = sampler
        .first_level
        .min(image.map_or(1, Image::mip_levels) - 1);
    let s = gpu::sampler_parameters(sampler);
    [
        op(state.texture_op),
        op(state.alpha_texture_op)
            | if image.is_some_and(|i| i.alpha_mode() == gpu::AlphaMode::Opaque) {
                1 << 31
            } else {
                0
            },
        compare,
        state.alpha_reference.to_bits(),
        s[0],
        s[1],
        s[2],
        s[3],
    ]
}

/// Prove full coverage only for two axis-aligned triangles with a shared
/// diagonal, or their four-vertex strip. Coverage lost to clipping or tests
/// never admits a discard load.
fn opaque_quad(
    vertices: &[Vertex],
    state: &DrawState,
    area: adv_gpu::Rect,
) -> Option<adv_gpu::Rect> {
    if state.blend.is_some()
        || state.alpha_blend.is_some()
        || state.color_write != 15
        || state.alpha_test != gpu::Compare::Always
        || state
            .depth
            .is_some_and(|compare| compare != gpu::Compare::Always)
        || state.stencil.is_some_and(|stencil| {
            stencil.front.compare != gpu::Compare::Always
                || stencil.back.compare != gpu::Compare::Always
        })
        || state.cull != gpu::Cull::None
    {
        return None;
    }
    let indices: &[[usize; 3]] = match (state.topology, vertices.len()) {
        (gpu::Topology::Triangles, 6) => &[[0, 1, 2], [3, 4, 5]],
        (gpu::Topology::TriangleStrip, 4) => &[[0, 1, 2], [2, 1, 3]],
        _ => return None,
    };
    if vertices
        .iter()
        .any(|v| v.position[3] != 1. || !(0. ..=1.).contains(&v.position[2]))
    {
        return None;
    }
    let v = state.viewport;
    let xy = |i: usize| {
        [
            (vertices[i].position[0] + 1.) * v.width * 0.5 + v.x,
            (1. - vertices[i].position[1]) * v.height * 0.5 + v.y,
        ]
    };
    let mut lo = [f32::INFINITY; 2];
    let mut hi = [f32::NEG_INFINITY; 2];
    for i in 0..vertices.len() {
        let p = xy(i);
        for a in 0..2 {
            lo[a] = lo[a].min(p[a]);
            hi[a] = hi[a].max(p[a]);
        }
    }
    if lo[0] >= hi[0] || lo[1] >= hi[1] {
        return None;
    }
    let mut masks = [0u8; 2];
    for (t, triangle) in indices.iter().enumerate() {
        for &i in triangle {
            let p = xy(i);
            let x = if p[0] == lo[0] {
                0
            } else if p[0] == hi[0] {
                1
            } else {
                return None;
            };
            let y = if p[1] == lo[1] {
                0
            } else if p[1] == hi[1] {
                1
            } else {
                return None;
            };
            let bit = 1 << (y * 2 + x);
            if masks[t] & bit != 0 {
                return None;
            }
            masks[t] |= bit;
        }
    }
    let shared = masks[0] & masks[1];
    if masks[0] | masks[1] != 15
        || !matches!(shared, 0b1001 | 0b0110)
        || lo[0] > area.x as f32
        || lo[1] > area.y as f32
        || hi[0] < (area.x + area.w) as f32
        || hi[1] < (area.y + area.h) as f32
    {
        return None;
    }
    Some(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: GFX-REC-1, GFX-PASS-1
    #[test]
    fn facade_clear_copy_readback_completion() {
        let mut gpu = match pollster::block_on(Gpu::request(!cfg!(target_vendor = "apple"))) {
            Ok(gpu) => gpu,
            Err(Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                return;
            }
            Err(error) => panic!("device: {error}"),
        };
        let make_image = |gpu: &Gpu| {
            let image = gpu
                .device
                .create_image(&backing::image_desc(
                    (4, 4),
                    adv_gpu::Format::Bgra8Unorm,
                    adv_gpu::resource::Usage::COLOR
                        | adv_gpu::resource::Usage::COPY_SRC
                        | adv_gpu::resource::Usage::COPY_DST
                        | adv_gpu::resource::Usage::SAMPLED,
                ))
                .unwrap();
            Image {
                storage: Storage::Fixed {
                    view: image.view().clone(),
                    image,
                    level: None,
                },
                alpha: gpu::AlphaMode::Stored,
            }
        };
        let source = make_image(&gpu);
        let destination = make_image(&gpu);
        let target = Target {
            image: source.clone(),
            depth: None,
        };
        let mut instanced = ProgrammableDraw {
            instance_count: 0,
            program: gpu::Program::new(Arc::from(""), Vec::new()),
            streams: vec![gpu::VertexStream {
                instance: true,
                bytes: vec![0; 16].into(),
                stride: 16,
                attributes: vec![gpu::Attribute {
                    location: 0,
                    offset: 0,
                    format: gpu::AttributeFormat::Float4,
                }],
            }],
            indices: None,
            vertex_count: 3,
            vertex_constants: Default::default(),
            pixel_constants: Default::default(),
            textures: Vec::new(),
            parameters: [[0; 4]; 40],
        };
        let state = DrawState::new(4, 4);
        assert!(matches!(
            gpu.draw_programmable(&target, &instanced, &state),
            Err(Error::Invalid(_))
        ));
        instanced.instance_count = 2;
        assert!(matches!(
            gpu.draw_programmable(&target, &instanced, &state),
            Err(Error::Invalid(_))
        ));
        let depth = gpu.depth(4, 4, gpu::DepthFormat::Float32).unwrap();
        gpu.clear(
            &Target {
                image: source.clone(),
                depth: Some(depth.clone()),
            },
            None,
            Some(1.),
        )
        .unwrap();
        gpu.discard_depth(&depth);
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        gpu.copy(&source, whole(&source), &destination, 0, 0)
            .unwrap();
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        let mut transfer = gpu.readback(&destination, Some(wake.clone())).unwrap();
        transfer.take_completion_job().unwrap()();
        assert!(transfer.take_completion_job().is_none());
        let pixels = transfer.poll().unwrap().unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 255, 255])
        );
        assert!(transfer.poll().is_none());
        assert!(wake.wait(Some(std::time::Duration::ZERO)).unwrap());
        let image = make_image(&gpu);
        let target = Target {
            image: image.clone(),
            depth: None,
        };
        gpu.clear(&target, Some([0., 1., 0., 1.]), None).unwrap();
        let mut desc = backing::image_desc(
            (4, 4),
            adv_gpu::Format::Bgra8Unorm,
            adv_gpu::resource::Usage::COLOR
                | adv_gpu::resource::Usage::COPY_SRC
                | adv_gpu::resource::Usage::COPY_DST,
        );
        desc.mips = 2;
        let mips = gpu.device.create_image(&desc).unwrap();
        let level = |n| Image {
            storage: Storage::Fixed {
                image: mips.clone(),
                view: mips
                    .create_view(&adv_gpu::resource::ViewDesc {
                        format: None,
                        dimension: None,
                        mips: n..n + 1,
                        layers: 0..1,
                        plane: 0,
                    })
                    .unwrap(),
                level: Some(n),
            },
            alpha: gpu::AlphaMode::Stored,
        };
        let (first, second) = (level(0), level(1));
        gpu.clear(
            &Target {
                image: first.clone(),
                depth: None,
            },
            Some([1., 0., 0., 1.]),
            None,
        )
        .unwrap();
        gpu.copy(
            &first,
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            &second,
            0,
            0,
        )
        .unwrap();
        let mut mip_readback = gpu.readback(&second, None).unwrap();
        mip_readback.take_completion_job().unwrap()();
        assert!(
            mip_readback
                .poll()
                .unwrap()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 0, 255, 255])
        );
        let mut surfaces =
            crate::surface::Surfaces::new(4, 4, 0, 4096, crate::surface::Snapshot::default())
                .unwrap();
        let id = surfaces.insert_native(target).unwrap();
        surfaces
            .present_native(id, Damage::Whole, &mut gpu)
            .unwrap();
        assert_eq!(surfaces.stats.presentations, 1);
        let mut transfer = surfaces.readback_native(id, &mut gpu).unwrap();
        transfer.take_completion_job().unwrap()();
        assert!(
            transfer
                .poll()
                .unwrap()
                .unwrap()
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [0, 255, 0, 255])
        );
    }

    // spec: GFX-REC-1, GFX-REC-5
    #[test]
    fn opacity_requires_full_coverage_and_no_fragment_rejection() {
        let v = |x, y| Vertex {
            position: [x, y, 0., 1.],
            color: [1.; 4],
            uv: [0.; 2],
        };
        let vertices = [
            v(-1., 1.),
            v(1., 1.),
            v(1., -1.),
            v(-1., 1.),
            v(1., -1.),
            v(-1., -1.),
        ];
        let mut state = DrawState::new(4, 4);
        let area = adv_gpu::Rect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        };
        assert_eq!(opaque_quad(&vertices, &state, area), Some(area));
        state.alpha_test = gpu::Compare::Greater;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        state.alpha_test = gpu::Compare::Always;
        state.color_write = 7;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        state.color_write = 15;
        let mut vertices = vertices;
        vertices[5].position[0] = 0.;
        assert_eq!(opaque_quad(&vertices, &state, area), None);
        assert!(validate_count(2, gpu::Topology::Triangles).is_err());
    }
}
