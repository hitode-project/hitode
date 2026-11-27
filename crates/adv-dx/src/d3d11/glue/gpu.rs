//! Resolve guest resources to typed GPU commands and order staging transfers.
use super::*;

mod draw;
mod gamma;
mod queries;
use crate::d3d11::{
    desc::{Texture, Usage},
    recording::{Boundary, Command, Effects, Recording, ResourceKey},
    resource::{self, Backing},
};
use adv_abi::types::D3D11_BOX;
use adv_cpu::backend::Guest;
use adv_device::{
    gpu::{BufferRange, Gpu, ModernTransfer, TransferBuffer},
    surface::{Access, Rect, Region},
};
use adv_gpu::{
    Buffer, Extent, ImageView,
    resource::{BufferDesc, BufferUsage, Memory},
};
use adv_machine::Ext;
pub(super) use draw::{
    dispatch, dispatch_indirect, draw, draw_auto, draw_indexed, draw_indexed_indirect,
    draw_indexed_instanced, draw_indirect, draw_instanced,
};
pub(super) use queries::{begin_query, end_query, get_data};
use std::sync::{Arc, mpsc::TryRecvError};

pub(super) struct Execution {
    gpu: Gpu,
    pub(super) recording: Recording,
    active_queries: BTreeMap<(u64, u64), u32>,
    present_target: Option<adv_device::gpu::Image>,
    present_gamma: Option<u64>,
    gamma: Option<(u64, gamma::Pass)>,
    verify_frames: bool,
    other_presentations: Option<u64>,
}

impl Execution {
    pub(super) fn new(gpu: Gpu) -> Self {
        Self {
            gpu,
            recording: Recording::default(),
            active_queries: BTreeMap::new(),
            present_target: None,
            present_gamma: None,
            gamma: None,
            verify_frames: std::env::var("ADV_VERIFY_FRAMES").as_deref() == Ok("1"),
            other_presentations: None,
        }
    }

    fn append(
        &mut self,
        command: Command,
        extent: Rect,
        read: Option<ResourceKey>,
        write: ResourceKey,
    ) -> Result<(), Error> {
        self.append_region(command, extent, extent, read, write)
    }

    fn append_region(
        &mut self,
        command: Command,
        extent: Rect,
        bounds: Rect,
        read: Option<ResourceKey>,
        write: ResourceKey,
    ) -> Result<(), Error> {
        let writes = [(write, false)];
        let key = 0;
        self.recording.append(
            &mut self.gpu,
            command,
            Effects {
                extent,
                bounds,
                key,
                reads: read.as_slice(),
                writes: &writes,
                retained_bytes: 0,
            },
        )?;
        Ok(())
    }

    fn barrier(&mut self, reason: Boundary) -> Result<(), Error> {
        self.recording.flush(&mut self.gpu, reason)?;
        Ok(())
    }
}

pub(super) fn execution(ext: &mut Ext) -> Result<&mut Execution, Error> {
    ext.get_mut::<State>()
        .execution
        .as_mut()
        .ok_or(Error::Device)
}

#[cfg(feature = "d3d9-counters")]
pub(super) fn count_map(ext: &mut Ext, constant_time: bool) {
    if let Ok(execution) = execution(ext) {
        execution.recording.counters.mapped(constant_time);
    }
}

#[cfg(feature = "d3d9-counters")]
pub fn frame_counters(ext: &Ext) -> Option<crate::d3d9::FrameCounters> {
    Some(
        ext.try_get::<State>()?
            .execution
            .as_ref()?
            .recording
            .counters,
    )
}

#[cfg(feature = "d3d9-counters")]
pub fn traffic(ext: &Ext) -> Option<adv_device::gpu::Traffic> {
    Some(ext.try_get::<State>()?.execution.as_ref()?.gpu.traffic())
}

fn context_device(env: &Env<'_>) -> Result<u32, Error> {
    context::owner(env)
}

fn null_device(env: &Env<'_>) -> Result<bool, Error> {
    let object = ops::object(env, table(env), context_device(env)?)?;
    Ok(object.device_state().unwrap().null_device)
}

fn description(env: &Env<'_>, pointer: u32) -> Result<Resource, Error> {
    context::owned(env, pointer)
        .and_then(Object::resource)
        .ok_or(Error::InvalidArgument)
}

fn key(env: &Env<'_>, pointer: u32, subresource: u32) -> ResourceKey {
    let identity = table(env)
        .allocation_key(&env.sys.objects, pointer)
        .unwrap();
    ResourceKey(identity, u64::from(subresource), 0)
}

pub(super) fn backing(
    env: &mut Env<'_>,
    pointer: u32,
    resource: Resource,
) -> Result<Backing, Error> {
    let table = table(env);
    let execution = execution(env.ext)?;
    let object = table.get_mut(&mut env.sys.objects, pointer).unwrap();
    if let Some(backing) = object.backing().unwrap() {
        return Ok(backing.clone());
    }
    let backing = Backing::new(&execution.gpu, resource)?;
    let initial = object.take_initial_data();
    for (index, bytes) in initial.iter().enumerate() {
        let transfer = match (&backing, resource) {
            (Backing::Buffer(buffer), Resource::Buffer(_)) => Some(ModernTransfer::CopyBuffer {
                source: BufferRange {
                    buffer: resource::upload(
                        &execution.gpu,
                        bytes,
                        BufferUsage::COPY_SRC,
                        buffer.desc().size,
                    )?,
                    range: 0..bytes.len() as u64,
                },
                destination: buffer.clone(),
                offset: 0,
            }),
            (_, Resource::Texture(texture)) => backing.upload_texture(
                &execution.gpu,
                texture,
                index as u32,
                [0; 3],
                texture.extent(index as u32 % texture.levels()),
                bytes,
            )?,
            _ => unreachable!(),
        };
        if let Some(transfer) = transfer {
            execution.gpu.transfer(transfer)?;
        }
    }
    if map::coherent(resource) {
        let storage = object.mapping().unwrap();
        for index in 0..resource.subresources() {
            storage.attach_staging_gpu(index);
        }
    }
    *object.backing().unwrap() = Some(backing.clone());
    Ok(backing)
}

pub(super) fn wait_staging(
    env: &mut Env<'_>,
    pointer: u32,
    subresource: u32,
    wait: bool,
) -> Result<(), Error> {
    let table = table(env);
    let pending = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .pending();
    let Some(completion) = pending.get(&subresource) else {
        return Ok(());
    };
    if wait {
        completion.recv().map_err(|_| Error::Device)?;
    } else {
        match completion.try_recv() {
            Ok(()) => (),
            Err(TryRecvError::Empty) => return Err(map::Error::StillDrawing.into()),
            Err(TryRecvError::Disconnected) => return Err(Error::Device),
        }
    }
    pending.remove(&subresource);
    Ok(())
}

fn pixel_region(
    image: &adv_gpu::Image,
    texture: Texture,
    subresource: u32,
    slice: u32,
    rect: Rect,
) -> adv_device::gpu::ImageRegion {
    let mut region = resource::region(image, texture, subresource);
    let format = texture.format();
    let (block_w, block_h) = (
        u32::from(format.alignment[0]),
        u32::from(format.alignment[1]),
    );
    let (x, y) = (rect.x * block_w, rect.y * block_h);
    region.origin = (x, y, slice);
    region.extent = Extent {
        width: (rect.w * block_w).min(region.extent.width - x),
        height: (rect.h * block_h).min(region.extent.height - y),
        depth_or_layers: 1,
    };
    region
}

pub(super) struct Shadow {
    pub base: u32,
    pub layout: map::Layout,
    pub slice: u32,
}

pub(super) fn download(
    ext: &mut Ext,
    cpu: &mut dyn Guest,
    backing: &Backing,
    resource: Resource,
    subresource: u32,
    shadow: Shadow,
    parts: &[Rect],
) -> Result<(), Error> {
    let Shadow {
        base,
        layout,
        slice,
    } = shadow;
    let execution = execution(ext)?;
    execution.barrier(Boundary::Map)?;
    for &rect in parts {
        let unit = match resource {
            Resource::Buffer(_) => 1,
            Resource::Texture(texture) => u32::from(texture.format().aligned_unit_bytes),
        };
        if let Backing::Blocks(blocks) = backing {
            let row = rect.w * unit;
            let packed = blocks.read(
                subresource,
                [rect.x * 4, rect.y * 4, slice],
                [rect.w * 4, rect.h * 4, 1],
            );
            for (line, bytes) in packed.chunks_exact(row as usize).enumerate() {
                let dst = layout.slice(base, slice)
                    + (rect.y + line as u32) * layout.row_pitch()
                    + rect.x * unit;
                cpu.write(dst, bytes).map_err(|_| Error::InvalidArgument)?;
            }
            continue;
        }
        let physical_unit = match (backing, resource) {
            (Backing::Image(image), Resource::Texture(texture)) => {
                resource::transfer_unit(texture, image.desc().format)
            }
            _ => unit,
        };
        let row = rect.w * physical_unit;
        let pitch = if let Backing::Image(image) = backing {
            resource::CopyPitch::new(image.desc().format, row).pitch
        } else {
            row.next_multiple_of(256)
        };
        let packed_depth = matches!(backing, Backing::Image(image)
            if image.desc().format.has_stencil());
        let buffer = execution.gpu.device().create_buffer(&BufferDesc {
            size: u64::from(pitch) * u64::from(rect.h),
            usage: if packed_depth {
                BufferUsage::STORAGE
            } else {
                BufferUsage::COPY_DST
            },
            memory: Memory::Readback,
            label: "D3D11 staging readback",
        })?;
        match (backing, resource) {
            (Backing::Image(image), Resource::Texture(texture)) if packed_depth => {
                depth_stencil::pack(
                    &mut execution.gpu,
                    resource::region(image, texture, subresource),
                    rect,
                    pitch,
                    buffer.clone(),
                    texture.format().parent == 44,
                )?;
            }
            _ => {
                let transfer = match (backing, resource) {
                    (Backing::Buffer(source), Resource::Buffer(_)) => ModernTransfer::CopyBuffer {
                        source: BufferRange {
                            buffer: source.clone(),
                            range: rect.x as u64..rect.x as u64 + u64::from(rect.w),
                        },
                        destination: buffer.clone(),
                        offset: 0,
                    },
                    (Backing::Image(image), Resource::Texture(texture)) => {
                        ModernTransfer::Download {
                            source: pixel_region(image, texture, subresource, slice, rect),
                            destination: TransferBuffer {
                                buffer: buffer.clone(),
                                offset: 0,
                                row_pitch: pitch,
                                rows: rect.h,
                            },
                        }
                    }
                    _ => unreachable!(),
                };
                execution.gpu.transfer(transfer)?;
            }
        }
        execution
            .gpu
            .completion()?
            .recv()
            .map_err(|_| Error::Device)?;
        let readback = buffer.mapped().unwrap();
        #[cfg(feature = "d3d9-counters")]
        execution
            .gpu
            .count_readback(u64::from(row) * u64::from(rect.h));
        // SAFETY: the transfer has finished and the readback allocation lives
        // through all copies into the guest shadow.
        let bytes = unsafe { readback.as_ref() };
        let mut packed = Vec::new();
        for line in 0..rect.h {
            let src = (line * pitch) as usize;
            let dst =
                layout.slice(base, slice) + (rect.y + line) * layout.row_pitch() + rect.x * unit;
            let mut row = &bytes[src..src + row as usize];
            if let (Backing::Image(image), Resource::Texture(texture)) = (backing, resource)
                && (physical_unit != unit || image.desc().format == adv_gpu::Format::Abgr4Unorm)
            {
                resource::readback_row(texture, image.desc().format, row, &mut packed);
                row = &packed;
            }
            cpu.write(dst, row).map_err(|_| Error::InvalidArgument)?;
        }
    }
    Ok(())
}

mod depth_stencil;

pub(super) fn staging_access(
    env: &mut Env<'_>,
    pointer: u32,
    resource: Resource,
    subresource: u32,
    backing: &Backing,
    access: Access,
) -> Result<(), Error> {
    if !map::coherent(resource) {
        return Ok(());
    }
    let key = key(env, pointer, subresource);
    let table = table(env);
    let store = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .mapping()
        .unwrap();
    let base = store.pointer();
    let mut transfer_error = None;
    let result = store.staging_gpu_access(subresource, access, |layout, slice, parts| {
        let result = (|| {
            let execution = execution(env.ext)?;
            if let Resource::Texture(texture) = resource {
                let [width, height, _] = texture.extent(subresource % texture.levels());
                execution
                    .recording
                    .written(key, extent_rect([width, height, 1]));
            }
            upload_shadow(
                env.ext,
                env.cpu,
                backing,
                resource,
                subresource,
                Shadow {
                    base,
                    layout,
                    slice,
                },
                parts,
            )
        })();
        result.map_err(|error| {
            transfer_error = Some(error);
            map::Error::InvalidArgument
        })
    });
    if let Some(error) = transfer_error {
        return Err(error);
    }
    Ok(result?)
}

fn upload_shadow(
    ext: &mut Ext,
    cpu: &dyn Guest,
    backing: &Backing,
    resource: Resource,
    subresource: u32,
    shadow: Shadow,
    parts: &[Rect],
) -> Result<(), Error> {
    let execution = execution(ext)?;
    execution.barrier(Boundary::Map)?;
    for &rect in parts {
        let unit = match resource {
            Resource::Buffer(_) => 1,
            Resource::Texture(texture) => u32::from(texture.format().aligned_unit_bytes),
        };
        let row = rect.w * unit;
        let mut bytes = vec![0u8; (row * rect.h) as usize];
        for (line, output) in bytes.chunks_exact_mut(row as usize).enumerate() {
            let address = shadow.layout.slice(shadow.base, shadow.slice)
                + (rect.y + line as u32) * shadow.layout.row_pitch()
                + rect.x * unit;
            cpu.read(address, output)
                .map_err(|_| Error::InvalidArgument)?;
        }
        let transfer = match (backing, resource) {
            (Backing::Buffer(destination), Resource::Buffer(_)) => ModernTransfer::CopyBuffer {
                source: BufferRange {
                    buffer: resource::upload(
                        &execution.gpu,
                        &bytes,
                        BufferUsage::COPY_SRC,
                        u64::from(row).next_multiple_of(4),
                    )?,
                    range: 0..u64::from(row),
                },
                destination: destination.clone(),
                offset: rect.x as u64,
            },
            (Backing::Image(image), Resource::Texture(texture)) => {
                let destination = pixel_region(image, texture, subresource, shadow.slice, rect);
                resource::texture_transfer(&execution.gpu, texture, destination, &bytes)?
            }
            (Backing::Blocks(blocks), Resource::Texture(_)) => {
                blocks.write(
                    subresource,
                    [rect.x * 4, rect.y * 4, shadow.slice],
                    [rect.w * 4, rect.h * 4, 1],
                    &bytes,
                );
                continue;
            }
            _ => unreachable!(),
        };
        execution.gpu.transfer(transfer)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Location {
    pointer: u32,
    subresource: u32,
    resource: Resource,
}

impl Location {
    fn new(env: &Env<'_>, pointer: u32, subresource: u32) -> Result<Self, Error> {
        let resource = description(env, pointer)?;
        if subresource >= resource.subresources() {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            pointer,
            subresource,
            resource,
        })
    }

    fn extent(self) -> [u32; 3] {
        match self.resource {
            Resource::Buffer(buffer) => [buffer.size(), 1, 1],
            Resource::Texture(texture) => texture.extent(self.subresource % texture.levels()),
        }
    }

    fn acquire(self, env: &mut Env<'_>, read: bool) -> Result<Backing, Error> {
        let backing = backing(env, self.pointer, self.resource)?;
        if read {
            staging_access(
                env,
                self.pointer,
                self.resource,
                self.subresource,
                &backing,
                Access::Read,
            )?;
        }
        Ok(backing)
    }

    fn written(self, env: &mut Env<'_>, backing: &Backing, whole: bool) -> Result<(), Error> {
        if !map::coherent(self.resource) {
            return Ok(());
        }
        if whole {
            let table = table(env);
            table
                .get_mut(&mut env.sys.objects, self.pointer)
                .unwrap()
                .mapping()
                .unwrap()
                .staging_gpu_overwrite(self.subresource);
        } else {
            staging_access(
                env,
                self.pointer,
                self.resource,
                self.subresource,
                backing,
                Access::Write,
            )?;
        }
        if matches!(backing, Backing::Blocks(_)) {
            return Ok(());
        }
        let execution = execution(env.ext)?;
        execution.barrier(Boundary::Map)?;
        let completion = execution.gpu.completion()?;
        let table = table(env);
        table
            .get_mut(&mut env.sys.objects, self.pointer)
            .unwrap()
            .pending()
            .insert(self.subresource, completion);
        Ok(())
    }
}

fn extent_rect(size: [u32; 3]) -> Rect {
    Rect {
        x: 0,
        y: 0,
        w: size[0],
        h: size[1],
    }
}

fn submit_copy(
    env: &mut Env<'_>,
    source: Location,
    destination: Location,
    origin: [u32; 3],
    at: [u32; 3],
    size: [u32; 3],
) -> Result<(), Error> {
    if size.contains(&0) {
        return Ok(());
    }
    let in_bounds = |origin: [u32; 3], extent: [u32; 3]| {
        (0..3).all(|axis| {
            origin[axis]
                .checked_add(size[axis])
                .is_some_and(|end| end <= extent[axis])
        })
    };
    if !in_bounds(origin, source.extent())
        || !in_bounds(at, destination.extent())
        || destination.resource.usage() == Usage::Immutable
        || (source.pointer == destination.pointer && source.subresource == destination.subresource)
    {
        return Err(Error::InvalidArgument);
    }
    let whole = at == [0; 3] && size == destination.extent();
    let src = source.acquire(env, true)?;
    let dest = destination.acquire(env, !whole)?;
    if let (
        Backing::Blocks(src),
        Backing::Blocks(dst),
        Resource::Texture(from),
        Resource::Texture(to),
    ) = (&src, &dest, source.resource, destination.resource)
    {
        if from.format().parent != to.format().parent
            || from.kind() != to.kind()
            || !aligned_box(from, source.extent(), origin, size)
            || !aligned_box(to, destination.extent(), at, size)
        {
            return Err(Error::InvalidArgument);
        }
        let bytes = src.read(source.subresource, origin, size);
        dst.write(destination.subresource, at, size, &bytes);
        return destination.written(env, &dest, whole);
    }
    let transfer = match (&src, &dest, source.resource, destination.resource) {
        (
            Backing::Buffer(source),
            Backing::Buffer(destination),
            Resource::Buffer(_),
            Resource::Buffer(_),
        ) => ModernTransfer::CopyBuffer {
            source: BufferRange {
                buffer: source.clone(),
                range: u64::from(origin[0])..u64::from(origin[0] + size[0]),
            },
            destination: destination.clone(),
            offset: u64::from(at[0]),
        },
        (
            Backing::Image(source_image),
            Backing::Image(destination_image),
            Resource::Texture(from),
            Resource::Texture(to),
        ) if from.format().parent == to.format().parent
            && from.kind() == to.kind()
            && from.samples() == to.samples()
            && aligned_box(from, source.extent(), origin, size)
            && aligned_box(to, destination.extent(), at, size) =>
        {
            let mut source_region = resource::region(source_image, from, source.subresource);
            source_region.origin = origin.into();
            source_region.extent = Extent {
                width: size[0],
                height: size[1],
                depth_or_layers: size[2],
            };
            let mut destination_region =
                resource::region(destination_image, to, destination.subresource);
            destination_region.origin = at.into();
            destination_region.extent = source_region.extent;
            if source_image.desc().format.has_depth() != destination_image.desc().format.has_depth()
            {
                if from.samples() != 1 {
                    return Err(Error::InvalidArgument);
                }
                let row = size[0] * u32::from(from.format().aligned_unit_bytes);
                let row_pitch = row.next_multiple_of(256);
                let rows = size[1];
                let buffer = execution(env.ext)?
                    .gpu
                    .device()
                    .create_buffer(&BufferDesc {
                        size: u64::from(row_pitch) * u64::from(rows) * u64::from(size[2]),
                        usage: BufferUsage::COPY_SRC | BufferUsage::COPY_DST,
                        memory: Memory::Device,
                        label: "D3D11 depth/color bit copy",
                    })?;
                ModernTransfer::CopyImageBits {
                    source: source_region,
                    scratch: TransferBuffer {
                        buffer,
                        offset: 0,
                        row_pitch,
                        rows,
                    },
                    destination: destination_region,
                }
            } else {
                ModernTransfer::CopyImage {
                    source: source_region,
                    destination: destination_region,
                }
            }
        }
        _ => return Err(Error::InvalidArgument),
    };
    let read = key(env, source.pointer, source.subresource);
    let write = key(env, destination.pointer, destination.subresource);
    execution(env.ext)?.append_region(
        Command::Transfer(transfer),
        extent_rect(destination.extent()),
        Rect {
            x: at[0],
            y: at[1],
            w: size[0],
            h: size[1],
        },
        Some(read),
        write,
    )?;
    destination.written(env, &dest, whole)
}

pub(super) fn copy_resource(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (destination, source) = (env.arg(1), env.arg(2));
    if description(env, source)?.subresources() != description(env, destination)?.subresources() {
        return Err(Error::InvalidArgument);
    }
    for subresource in 0..description(env, source)?.subresources() {
        let source = Location::new(env, source, subresource)?;
        let destination = Location::new(env, destination, subresource)?;
        let size = source.extent();
        if destination.extent() != size {
            return Err(Error::InvalidArgument);
        }
        submit_copy(env, source, destination, [0; 3], [0; 3], size)?;
    }
    Ok(())
}

pub(super) fn write_dynamic(
    env: &mut Env<'_>,
    pointer: u32,
    subresource: u32,
    bytes: &[u8],
) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let location = Location::new(env, pointer, subresource)?;
    let size = location.extent();
    let table = table(env);
    let shadow = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .mapping()
        .unwrap();
    let layout = shadow.layout(subresource);
    let base = layout.slice(shadow.pointer(), 0);
    env.cpu
        .write(base, bytes)
        .map_err(|_| Error::InvalidArgument)?;
    let backing = location.acquire(env, false)?;
    let gpu = &execution(env.ext)?.gpu;
    let transfer = match (&backing, location.resource) {
        (Backing::Buffer(buffer), Resource::Buffer(_)) => Some(ModernTransfer::CopyBuffer {
            source: BufferRange {
                buffer: resource::upload(gpu, bytes, BufferUsage::COPY_SRC, buffer.desc().size)?,
                range: 0..bytes.len() as u64,
            },
            destination: buffer.clone(),
            offset: 0,
        }),
        (_, Resource::Texture(texture)) => {
            let format = texture.format();
            let row = size[0].div_ceil(u32::from(format.alignment[0]))
                * u32::from(format.aligned_unit_bytes);
            let rows = size[1].div_ceil(u32::from(format.alignment[1]));
            let mut packed = Vec::with_capacity((row * rows * size[2]) as usize);
            for z in 0..size[2] {
                for y in 0..rows {
                    let offset = (z * layout.depth_pitch() + y * layout.row_pitch()) as usize;
                    packed.extend_from_slice(&bytes[offset..offset + row as usize]);
                }
            }
            backing.upload_texture(gpu, texture, subresource, [0; 3], size, &packed)?
        }
        _ => unreachable!(),
    };
    if let Some(transfer) = transfer {
        let write = key(env, pointer, subresource);
        execution(env.ext)?.append(Command::Transfer(transfer), extent_rect(size), None, write)?;
    }
    Ok(())
}

fn aligned_box(texture: Texture, full: [u32; 3], at: [u32; 3], size: [u32; 3]) -> bool {
    (0..3).all(|axis| {
        let align = u32::from(texture.format().alignment[axis]);
        at[axis].is_multiple_of(align)
            && (size[axis].is_multiple_of(align) || at[axis] + size[axis] == full[axis])
    })
}

fn box_extent(input: D3D11_BOX) -> ([u32; 3], [u32; 3]) {
    (
        [input.left, input.top, input.front],
        [
            input.right.saturating_sub(input.left),
            input.bottom.saturating_sub(input.top),
            input.back.saturating_sub(input.front),
        ],
    )
}

pub(super) fn copy_region(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let destination = Location::new(env, env.arg(1), env.arg(2))?;
    let at = [env.arg(3), env.arg(4), env.arg(5)];
    let source = Location::new(env, env.arg(6), env.arg(7))?;
    let area = env.arg(8);
    let (origin, size) = if area == 0 {
        ([0; 3], source.extent())
    } else {
        box_extent(read(env, area)?)
    };
    submit_copy(env, source, destination, origin, at, size)
}

pub(super) fn update_resource(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let destination = Location::new(env, env.arg(1), env.arg(2))?;
    let area = env.arg(3);
    let data = env.arg(4);
    let (row_pitch, slice_pitch) = (env.arg(5), env.arg(6));
    let (origin, size) = if area == 0 {
        ([0; 3], destination.extent())
    } else {
        box_extent(read(env, area)?)
    };
    if size.contains(&0) {
        return Ok(());
    }
    let extent = destination.extent();
    if (0..3).any(|i| {
        origin[i]
            .checked_add(size[i])
            .is_none_or(|end| end > extent[i])
    }) || destination.resource.usage() != Usage::Default
    {
        return Err(Error::InvalidArgument);
    }
    if let Resource::Texture(texture) = destination.resource
        && !aligned_box(texture, extent, origin, size)
    {
        return Err(Error::InvalidArgument);
    }
    let whole = origin == [0; 3] && size == extent;
    let backing = destination.acquire(env, !whole)?;
    let transfer = match (&backing, destination.resource) {
        (Backing::Buffer(buffer), Resource::Buffer(_)) => {
            let bytes = array::<u8>(env, data, size[0])?;
            let source = resource::upload(
                &execution(env.ext)?.gpu,
                &bytes,
                BufferUsage::COPY_SRC,
                u64::from(size[0]).next_multiple_of(4),
            )?;
            Some(ModernTransfer::CopyBuffer {
                source: BufferRange {
                    buffer: source,
                    range: 0..u64::from(size[0]),
                },
                destination: buffer.clone(),
                offset: u64::from(origin[0]),
            })
        }
        (_, Resource::Texture(texture)) => {
            let format = texture.format();
            let row = size[0].div_ceil(u32::from(format.alignment[0]))
                * u32::from(format.aligned_unit_bytes);
            let rows = size[1].div_ceil(u32::from(format.alignment[1]));
            let mut packed = Vec::new();
            for z in 0..size[2] {
                for y in 0..rows {
                    let offset =
                        u64::from(z) * u64::from(slice_pitch) + u64::from(y) * u64::from(row_pitch);
                    let addr = u32::try_from(u64::from(data) + offset)
                        .map_err(|_| Error::InvalidArgument)?;
                    packed.extend(array::<u8>(env, addr, row)?);
                }
            }
            backing.upload_texture(
                &execution(env.ext)?.gpu,
                texture,
                destination.subresource,
                origin,
                size,
                &packed,
            )?
        }
        _ => unreachable!(),
    };
    if let Some(transfer) = transfer {
        let write = key(env, destination.pointer, destination.subresource);
        execution(env.ext)?.append_region(
            Command::Transfer(transfer),
            extent_rect(extent),
            Rect {
                x: origin[0],
                y: origin[1],
                w: size[0],
                h: size[1],
            },
            None,
            write,
        )
    } else {
        destination.written(env, &backing, whole)
    }
}

fn image_view(
    env: &mut Env<'_>,
    pointer: u32,
    kind: ViewKind,
) -> Result<(ImageView, ResourceKey), Error> {
    let (resource, view) = context::owned(env, pointer)
        .and_then(Object::view)
        .ok_or(Error::InvalidArgument)?;
    if view.kind() != kind {
        return Err(Error::InvalidArgument);
    }
    let view = view.clone();
    let desc = description(env, resource)?;
    let subresource = view.subresources().mips.start;
    let key = key(env, resource, subresource);
    let backing = backing(env, resource, desc)?;
    if map::coherent(desc) {
        let table = table(env);
        let object = table.get_mut(&mut env.sys.objects, resource).unwrap();
        if *object.dc().unwrap() != 0 {
            return Err(Error::InvalidArgument);
        }
        let access = if kind == ViewKind::ShaderResource {
            Access::Read
        } else {
            Access::Write
        };
        staging_access(env, resource, desc, subresource, &backing, access)?;
    }
    Ok((backing.view(&execution(env.ext)?.gpu, &view)?, key))
}

fn view_rect(view: &ImageView) -> Rect {
    let extent = view.size();
    extent_rect([extent.width, extent.height, 1])
}

pub(super) fn clear_color(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (pointer, value) = (env.arg(1), read::<[f32; 4]>(env, env.arg(2))?);
    let (view, key) = image_view(env, pointer, ViewKind::RenderTarget)?;
    let extent = view_rect(&view);
    for view in clear_slices(env, pointer, view)? {
        let command = Command::Transfer(ModernTransfer::ClearColor {
            value: color_clear_value(&view, value.map(f64::from)),
            view,
            area: None,
        });
        execution(env.ext)?.append(command, extent, None, key)?;
    }
    Ok(())
}

fn clear_slices(env: &Env<'_>, pointer: u32, view: ImageView) -> Result<Vec<ImageView>, Error> {
    let (_, desc) = context::owned(env, pointer).and_then(Object::view).unwrap();
    let layers = desc.attachment_layers().len();
    if layers == 1 {
        return Ok(vec![view]);
    }
    (0..layers as u32)
        .map(|layer| draw::attachment_layer(&view, desc, layer))
        .collect()
}

pub(super) fn clear_depth(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (pointer, flags, depth, stencil) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let (view, key) = image_view(env, pointer, ViewKind::DepthStencil)?;
    let extent = view_rect(&view);
    for view in clear_slices(env, pointer, view)? {
        let command = Command::Transfer(ModernTransfer::ClearDepth {
            view,
            depth: (flags & 1 != 0).then(|| f32::from_bits(depth).clamp(0., 1.)),
            stencil: (flags & 2 != 0).then_some(stencil & 255),
        });
        execution(env.ext)?.append(command, extent, None, key)?;
    }
    Ok(())
}

fn resolve_view(
    env: &mut Env<'_>,
    location: Location,
    format: adv_gpu::Format,
) -> Result<ImageView, Error> {
    let Backing::Image(image) = location.acquire(env, false)? else {
        return Err(Error::InvalidArgument);
    };
    let Resource::Texture(texture) = location.resource else {
        return Err(Error::InvalidArgument);
    };
    let region = resource::region(&image, texture, location.subresource);
    let format = if format == adv_gpu::Format::A8Unorm {
        adv_gpu::Format::R8Unorm
    } else {
        format
    };
    Ok(image.create_view(&adv_gpu::resource::ViewDesc {
        format: (image.desc().format != format).then_some(format),
        dimension: None,
        mips: region.mip..region.mip + 1,
        layers: region.layer..region.layer + 1,
        plane: 0,
    })?)
}

pub(super) fn resolve(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let dest = Location::new(env, env.arg(1), env.arg(2))?;
    let source = Location::new(env, env.arg(3), env.arg(4))?;
    let dxgi = env.arg(5);
    let format = resource::backing_format(execution(env.ext)?.gpu.device().caps(), dxgi)
        .ok_or(Error::InvalidArgument)?;
    let source_view = resolve_view(env, source, format)?;
    let destination = resolve_view(env, dest, format)?;
    let read = key(env, source.pointer, source.subresource);
    let write = key(env, dest.pointer, dest.subresource);
    let extent = view_rect(&destination);
    let caps = execution(env.ext)?.gpu.device().caps();
    if let Some(maxima) = resource::packed_resolve(caps, dxgi, format) {
        use adv_device::gpu::{ModernDraw, SlotBindings};
        let draw = ModernDraw::packed_resolve(destination, maxima)?;
        return execution(env.ext)?.append(
            Command::Draw {
                draw: Box::new(draw),
                vertex: SlotBindings::default(),
                fragment: SlotBindings {
                    images: vec![Some(source_view)],
                    ..Default::default()
                },
                statistics: None,
            },
            extent,
            Some(read),
            write,
        );
    }
    if cfg!(target_vendor = "apple") && resource::image_filter_needed(caps, format) {
        let execution = execution(env.ext)?;
        let pipeline = Arc::new(execution.gpu.prepare_image_filter(format, false, true)?);
        return filter_image(execution, pipeline, source_view, destination, read, write);
    }
    execution(env.ext)?.append(
        Command::Resolve {
            source: source_view,
            destination,
        },
        extent,
        Some(read),
        write,
    )
}

pub(super) fn generate_mips(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let pointer = env.arg(1);
    let (mut view, key) = image_view(env, pointer, ViewKind::ShaderResource)?;
    let format = view.format();
    if resource::image_filter_needed(execution(env.ext)?.gpu.device().caps(), format) {
        let (resource, desc) = context::owned(env, pointer).and_then(Object::view).unwrap();
        let range = desc.subresources().clone();
        let Resource::Texture(texture) = description(env, resource)? else {
            unreachable!()
        };
        let image = view.image();
        let dimension = image.desc().dimension;
        let volume = dimension == adv_gpu::resource::Dimension::D3;
        let pipeline = Arc::new(
            execution(env.ext)?
                .gpu
                .prepare_image_filter(format, volume, false)?,
        );
        for layer in range.layers {
            for mip in range.mips.start + 1..range.mips.end {
                let slice = |mip| {
                    image.create_view(&adv_gpu::resource::ViewDesc {
                        format: (format != image.desc().format).then_some(format),
                        dimension: (dimension == adv_gpu::resource::Dimension::Cube)
                            .then_some(adv_gpu::resource::Dimension::D2),
                        mips: mip..mip + 1,
                        layers: layer..layer + 1,
                        plane: 0,
                    })
                };
                let subresource = layer * texture.levels() + mip;
                let read = self::key(env, resource, subresource - 1);
                let write = self::key(env, resource, subresource);
                filter_image(
                    execution(env.ext)?,
                    pipeline.clone(),
                    slice(mip - 1)?,
                    slice(mip)?,
                    read,
                    write,
                )?;
            }
        }
        return Ok(());
    }
    if view.format() == adv_gpu::Format::A8Unorm {
        let (_, desc) = context::owned(env, pointer).and_then(Object::view).unwrap();
        view = resource::image_view(view.image(), desc, adv_gpu::Format::R8Unorm)?;
    }
    let extent = view_rect(&view);
    execution(env.ext)?.append(Command::GenerateMips(view), extent, Some(key), key)
}

fn filter_image(
    execution: &mut Execution,
    pipeline: Arc<adv_device::gpu::Dispatch>,
    source: ImageView,
    destination: ImageView,
    read: ResourceKey,
    write: ResourceKey,
) -> Result<(), Error> {
    use adv_device::gpu::{DispatchArguments, SlotBindings, StorageBinding};
    let size = destination.size();
    let volume = destination.image().desc().dimension == adv_gpu::resource::Dimension::D3;
    let group = if volume { 4 } else { 8 };
    execution.append(
        Command::Dispatch {
            pipeline,
            bindings: SlotBindings {
                images: vec![Some(source)],
                storage: vec![Some(StorageBinding::Image(destination))],
                ..Default::default()
            },
            arguments: DispatchArguments::Direct([
                size.width.div_ceil(group),
                size.height.div_ceil(group),
                if volume {
                    size.depth_or_layers.div_ceil(4)
                } else {
                    1
                },
            ]),
        },
        extent_rect([size.width, size.height, 1]),
        Some(read),
        write,
    )
}

pub(super) fn flush_shared(env: &mut Env<'_>) -> Result<(), Error> {
    execution(env.ext)?.barrier(Boundary::Flush)
}

pub(super) fn flush(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    execution(env.ext)?.barrier(Boundary::Flush)
}

pub(super) fn clear_uav_uint(env: &mut Env<'_>) -> Result<(), Error> {
    clear_uav(env, true)
}

pub(super) fn clear_uav_float(env: &mut Env<'_>) -> Result<(), Error> {
    clear_uav(env, false)
}

fn clear_uav(env: &mut Env<'_>, integer: bool) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (pointer, raw) = (env.arg(1), read::<[u32; 4]>(env, env.arg(2))?);
    let (pointer, view) = context::owned(env, pointer)
        .and_then(Object::view)
        .ok_or(Error::InvalidArgument)?;
    let view = view.clone();
    if view.kind() != ViewKind::UnorderedAccess {
        return Err(Error::InvalidArgument);
    }
    let desc = description(env, pointer)?;
    let key = key(env, pointer, view.subresources().mips.start);
    let backing = backing(env, pointer, desc)?;
    uav::clear(execution(env.ext)?, key, desc, &view, backing, raw, integer)
}

mod uav;

pub(super) fn caps(ext: &Ext) -> Option<&adv_gpu::Caps> {
    ext.get::<State>()
        .execution
        .as_ref()
        .map(|execution| execution.gpu.device().caps())
}

pub(super) fn present(
    env: &mut Env<'_>,
    pointer: u32,
    source: [u32; 2],
    serial: u64,
    first: bool,
) -> Result<bool, adv_device::gpu::Error> {
    use adv_device::gpu::{Error as GpuError, Filter, Image};
    use adv_device::surface::SurfaceStore;
    let error = |error: Error| GpuError::Device(format!("D3D11 present: {:#x}", error.hresult()));
    let object = ops::object(env, table(env), pointer).map_err(error)?;
    let description = object
        .resource()
        .ok_or_else(|| error(Error::InvalidArgument))?;
    let device = object.device().unwrap();
    let Backing::Image(mut image) = backing(env, pointer, description).map_err(error)? else {
        return Err(error(Error::InvalidArgument));
    };
    let key = key(env, pointer, 0);
    let size = (source[0], source[1]);
    let other_presentations = env
        .ext
        .try_get::<SurfaceStore>()
        .map(|store| store.stats.presentations);
    let gamma = crate::dxgi::output_gamma(env.ext, device);
    let revision = gamma.map(|(revision, _)| revision);
    let gamma_update = gamma
        .filter(|(revision, _)| {
            env.ext
                .get::<State>()
                .execution
                .as_ref()
                .unwrap()
                .gamma
                .as_ref()
                .is_none_or(|(old, pass)| old != revision || pass.size() != size)
        })
        .map(|(_, curve)| *curve);
    let execution = execution(env.ext).map_err(error)?;
    let damage = if first
        || execution.other_presentations != other_presentations
        || execution.present_gamma != revision
    {
        Some(Region::whole(size))
    } else {
        execution.recording.damage(key, size)
    };
    let pixels = damage.as_ref().map_or(0, Region::area);
    let publish = damage.is_some();
    if publish {
        execution.gpu.try_admit_present()?;
    }
    if let Some(curve) = gamma_update {
        let pass = gamma::Pass::new(&execution.gpu, size, curve).map_err(error)?;
        execution.gamma = Some((revision.unwrap(), pass));
    } else if revision.is_none() {
        execution.gamma = None;
    }
    let target = execution
        .present_target
        .as_ref()
        .filter(|image| (image.width(), image.height()) == size)
        .cloned();
    let (target, submitted) = execution.gpu.with_nonblocking_admission(|gpu| {
        let commands = execution.recording.submit(gpu)?;
        if !publish && !execution.verify_frames {
            if commands {
                gpu.flush()?;
            }
            return Ok((None, commands));
        }
        if image.desc().samples > 1 {
            let resolved = gpu
                .device()
                .create_image(&adv_gpu::resource::ImageDesc {
                    samples: 1,
                    usage: adv_gpu::resource::Usage::COLOR
                        | adv_gpu::resource::Usage::SAMPLED
                        | adv_gpu::resource::Usage::COPY_SRC,
                    ..*image.desc()
                })
                .map_err(|error| GpuError::Device(error.to_string()))?;
            gpu.resolve(image.view().clone(), resolved.view().clone())?;
            image = resolved;
        }
        let target = match target {
            Some(target) => target,
            None => gpu.presentation_target(size.0, size.1, false)?.image,
        };
        let copies = if execution.verify_frames {
            vec![extent_rect([size.0, size.1, 1]).into()]
        } else {
            damage.as_ref().unwrap().rects()
        };
        let source = if let Some((_, pass)) = &execution.gamma {
            pass.apply(gpu, &image, &copies)?;
            pass.image.clone()
        } else {
            Image::from_native(image)
        };
        for rect in copies {
            gpu.blit(&source, rect.into(), &target, rect.into(), Filter::Nearest)?;
        }
        if execution.verify_frames {
            let changed = damage.clone().unwrap_or_else(|| Region::empty(size));
            execution.recording.verify(gpu, &target, changed)?;
        }
        if let Some(damage) = damage {
            gpu.present_admitted(
                &target,
                damage,
                serial,
                std::sync::Arc::new([]),
                std::sync::Arc::new([]),
            );
        }
        Ok((Some(target), true))
    })?;
    if let Some(target) = target {
        execution.present_target = Some(target);
        execution.other_presentations = other_presentations;
        execution.present_gamma = revision;
    }
    execution
        .recording
        .presented(key, size, serial, pixels, submitted);
    Ok(publish)
}

pub(super) fn capture(ext: &mut Ext) -> Result<adv_device::frame::Waker, Error> {
    Ok(execution(ext)?.gpu.waker())
}

pub(super) fn copy_capture(
    env: &mut Env<'_>,
    pointer: u32,
    description: Resource,
    frame: &adv_device::frame::Captured,
) -> Result<(), Error> {
    use adv_device::gpu::Image;
    use adv_gpu::resource::Usage as GpuUsage;
    let Resource::Texture(texture) = description else {
        return Err(Error::InvalidArgument);
    };
    if texture.layers() != 1 || texture.levels() != 1 || texture.samples() != 1 {
        return Err(Error::InvalidArgument);
    }
    let location = Location {
        pointer,
        subresource: 0,
        resource: description,
    };
    let dest = location.acquire(env, false)?;
    let Backing::Image(image) = &dest else {
        return Err(Error::InvalidArgument);
    };
    let execution = execution(env.ext)?;
    execution.recording.submit(&mut execution.gpu)?;
    let gpu = &mut execution.gpu;
    let source = gpu.image(frame.width, frame.height)?;
    let source_rect = extent_rect([frame.width, frame.height, 1]);
    gpu.upload(&source, source_rect, frame.width * 4, &frame.bgra)?;
    let converted = gpu.device().create_image(&adv_gpu::resource::ImageDesc {
        usage: GpuUsage::COLOR | GpuUsage::COPY_SRC,
        ..*image.desc()
    })?;
    gpu.point_copy(&source, &Image::from_native(converted.clone()))?;
    gpu.transfer(ModernTransfer::CopyImage {
        source: resource::region(&converted, texture, 0),
        destination: resource::region(image, texture, 0),
    })?;
    location.written(env, &dest, true)
}

pub(super) fn clear_view(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (pointer, value, rectangles, count) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let value = read::<[f32; 4]>(env, value)?.map(f64::from);
    let kind = context::owned(env, pointer)
        .and_then(Object::view)
        .map(|(_, view)| view.kind())
        .ok_or(Error::InvalidArgument)?;
    if matches!(kind, ViewKind::DepthStencil | ViewKind::ShaderResource) {
        return Err(Error::InvalidArgument);
    }
    let (view, key) = image_view(env, pointer, kind)?;
    let extent = view_rect(&view);
    let rectangles = if count == 0 {
        vec![extent]
    } else {
        array::<RECT>(env, rectangles, count)?
            .into_iter()
            .map(|rect| {
                let left = rect.left.max(0);
                let top = rect.top.max(0);
                Rect {
                    x: left as u32,
                    y: top as u32,
                    w: rect.right.saturating_sub(left).max(0) as u32,
                    h: rect.bottom.saturating_sub(top).max(0) as u32,
                }
                .clipped(extent)
            })
            .collect()
    };
    let views = clear_slices(env, pointer, view)?;
    let execution = execution(env.ext)?;
    for rect in rectangles.into_iter().filter(|rect| !rect.empty()) {
        for view in &views {
            let writes = [(key, false)];
            let command = Command::Transfer(ModernTransfer::ClearColor {
                view: view.clone(),
                value: color_clear_value(view, value),
                area: Some(rect.into()),
            });
            execution.recording.append(
                &mut execution.gpu,
                command,
                Effects {
                    extent,
                    bounds: rect,
                    key: 0,
                    reads: &[],
                    writes: &writes,
                    retained_bytes: 0,
                },
            )?;
        }
    }
    Ok(())
}

fn color_clear_value(view: &ImageView, mut value: [f64; 4]) -> [f64; 4] {
    if view.image().desc().format == adv_gpu::Format::A8Unorm {
        value[0] = value[3];
    }
    value
}

pub(super) fn check(ext: &Ext) -> Result<(), Error> {
    if let Some(execution) = ext.get::<State>().execution.as_ref() {
        execution.gpu.check()?;
    }
    Ok(())
}

fn stream_cursor(env: &mut Env<'_>, pointer: u32) -> Result<BufferRange, Error> {
    let cursor = table(env)
        .get_mut(&mut env.sys.objects, pointer)
        .and_then(Object::stream_cursor)
        .ok_or(Error::InvalidArgument)?;
    let buffer = if let Some(buffer) = cursor {
        buffer.clone()
    } else {
        let usage = BufferUsage::STORAGE | BufferUsage::COPY_DST;
        let buffer = resource::upload(&execution(env.ext)?.gpu, &[], usage, 4)?;
        *cursor = Some(buffer.clone());
        buffer
    };
    Ok(BufferRange {
        buffer,
        range: 0..4,
    })
}

pub(super) fn reset_stream_cursors(
    env: &mut Env<'_>,
    output: &super::super::state::StreamOutput,
) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    for (&pointer, offset) in output.buffers.iter().zip(output.offsets) {
        if pointer == 0 || offset == u32::MAX {
            continue;
        }
        let cursor = stream_cursor(env, pointer)?;
        let key = key(env, pointer, 0);
        let transfer = ModernTransfer::FillBuffer {
            destination: cursor,
            value: offset,
        };
        execution(env.ext)?.append(
            Command::Transfer(transfer),
            extent_rect([4, 1, 1]),
            None,
            key,
        )?;
    }
    Ok(())
}

pub(super) fn counter_buffer(env: &mut Env<'_>, pointer: u32) -> Result<Buffer, Error> {
    let table = table(env);
    let counter = table
        .get_mut(&mut env.sys.objects, pointer)
        .and_then(Object::counter)
        .ok_or(Error::InvalidArgument)?;
    if let Some(buffer) = counter {
        return Ok(buffer.clone());
    }
    let usage = BufferUsage::STORAGE | BufferUsage::COPY_SRC | BufferUsage::COPY_DST;
    let buffer = resource::upload(&execution(env.ext)?.gpu, &[], usage, 4)?;
    *counter = Some(buffer.clone());
    Ok(buffer)
}

pub(super) fn reset_counters(
    env: &mut Env<'_>,
    pointers: &[u32],
    initial: u32,
) -> Result<(), Error> {
    if initial == 0 || pointers.is_empty() {
        return Ok(());
    }
    let values = array::<u32>(env, initial, pointers.len() as u32)?;
    reset_counter_values(env, pointers.iter().copied().zip(values))
}

pub(super) fn reset_counter_values(
    env: &mut Env<'_>,
    counters: impl IntoIterator<Item = (u32, u32)>,
) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    for (pointer, value) in counters {
        if value == u32::MAX
            || !context::owned(env, pointer)
                .and_then(Object::view)
                .is_some_and(|(_, view)| view.has_counter())
        {
            continue;
        }
        let buffer = counter_buffer(env, pointer)?;
        let key = key(env, pointer, 0);
        let command = Command::Transfer(ModernTransfer::FillBuffer {
            destination: BufferRange {
                buffer,
                range: 0..4,
            },
            value,
        });
        execution(env.ext)?.append(command, extent_rect([4, 1, 1]), None, key)?;
    }
    Ok(())
}

pub(super) fn copy_structure_count(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (destination, offset, view) = (env.arg(1), env.arg(2), env.arg(3));
    if !context::owned(env, view)
        .and_then(Object::view)
        .is_some_and(|(_, view)| view.has_counter())
    {
        return Err(Error::InvalidArgument);
    }
    let desc = description(env, destination)?;
    let Resource::Buffer(buffer) = desc else {
        return Err(Error::InvalidArgument);
    };
    if offset % 4 != 0
        || offset.checked_add(4).is_none_or(|end| end > buffer.size())
        || desc.usage() == Usage::Immutable
    {
        return Err(Error::InvalidArgument);
    }
    let location = Location::new(env, destination, 0)?;
    let backing = location.acquire(env, true)?;
    let Backing::Buffer(buffer) = &backing else {
        unreachable!()
    };
    let counter = counter_buffer(env, view)?;
    let source = key(env, view, 0);
    let target = key(env, destination, 0);
    let command = Command::Transfer(ModernTransfer::CopyBuffer {
        source: BufferRange {
            buffer: counter,
            range: 0..4,
        },
        destination: buffer.clone(),
        offset: u64::from(offset),
    });
    execution(env.ext)?.append(
        command,
        extent_rect(location.extent()),
        Some(source),
        target,
    )?;
    location.written(env, &backing, false)
}
