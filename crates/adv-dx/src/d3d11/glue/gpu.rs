//! Resolve guest resources to typed GPU commands and order staging transfers.
use super::*;

mod draw;
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
    surface::{Access, Rect},
};
use adv_gpu::{
    Buffer, Extent, ImageView,
    resource::{BufferDesc, BufferUsage, Memory},
};
use adv_machine::Ext;
pub(super) use draw::{
    dispatch, dispatch_indirect, draw, draw_indexed, draw_indexed_indirect, draw_indexed_instanced,
    draw_indirect, draw_instanced,
};
pub(super) use queries::{begin_query, end_query, get_data};
use std::sync::mpsc::TryRecvError;

pub(super) struct Execution {
    gpu: Gpu,
    recording: Recording,
    active_queries: BTreeMap<(u64, u64), u32>,
    present_target: Option<adv_device::gpu::Image>,
}

impl Execution {
    pub(super) fn new(gpu: Gpu) -> Self {
        Self {
            gpu,
            recording: Recording::default(),
            active_queries: BTreeMap::new(),
            present_target: None,
        }
    }

    fn append(
        &mut self,
        command: Command,
        extent: Rect,
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
                bounds: extent,
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
    ResourceKey(
        table(env).key(&env.sys.objects, pointer).unwrap(),
        u64::from(subresource),
        0,
    )
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
            (Backing::Buffer(buffer), Resource::Buffer(_)) => ModernTransfer::CopyBuffer {
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
            },
            (Backing::Image(image), Resource::Texture(texture)) => {
                let destination = resource::region(image, texture, index as u32);
                ModernTransfer::Upload {
                    source: resource::texture_transfer(
                        &execution.gpu,
                        texture,
                        &destination,
                        bytes,
                    )?,
                    destination,
                }
            }
            _ => unreachable!(),
        };
        execution.gpu.transfer(transfer)?;
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
        let row = rect.w * unit;
        let pitch = row.next_multiple_of(256);
        let buffer = execution.gpu.device().create_buffer(&BufferDesc {
            size: u64::from(pitch) * u64::from(rect.h),
            usage: BufferUsage::COPY_DST,
            memory: Memory::Readback,
            label: "D3D11 staging readback",
        })?;
        let transfer = match (backing, resource) {
            (Backing::Buffer(source), Resource::Buffer(_)) => ModernTransfer::CopyBuffer {
                source: BufferRange {
                    buffer: source.clone(),
                    range: rect.x as u64..rect.x as u64 + u64::from(rect.w),
                },
                destination: buffer.clone(),
                offset: 0,
            },
            (Backing::Image(image), Resource::Texture(texture)) => ModernTransfer::Download {
                source: pixel_region(image, texture, subresource, slice, rect),
                destination: TransferBuffer {
                    buffer: buffer.clone(),
                    offset: 0,
                    row_pitch: pitch,
                    rows: rect.h,
                },
            },
            _ => unreachable!(),
        };
        execution.gpu.transfer(transfer)?;
        execution
            .gpu
            .completion()?
            .recv()
            .map_err(|_| Error::Device)?;
        let readback = buffer.mapped().unwrap();
        // SAFETY: the transfer has finished and the readback allocation lives
        // through all copies into the guest shadow.
        let bytes = unsafe { readback.as_ref() };
        for line in 0..rect.h {
            let src = (line * pitch) as usize;
            let dst =
                layout.slice(base, slice) + (rect.y + line) * layout.row_pitch() + rect.x * unit;
            cpu.write(dst, &bytes[src..src + row as usize])
                .map_err(|_| Error::InvalidArgument)?;
        }
    }
    Ok(())
}

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
    let table = table(env);
    let store = table
        .get_mut(&mut env.sys.objects, pointer)
        .unwrap()
        .mapping()
        .unwrap();
    let base = store.pointer();
    let mut transfer_error = None;
    let result = store.staging_gpu_access(subresource, access, |layout, slice, parts| {
        let result = upload_shadow(
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
        );
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
                let source =
                    resource::texture_transfer(&execution.gpu, texture, &destination, &bytes)?;
                ModernTransfer::Upload {
                    source,
                    destination,
                }
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
        ) if from.format().parent == to.format().parent && from.samples() == to.samples() => {
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
            ModernTransfer::CopyImage {
                source: source_region,
                destination: destination_region,
            }
        }
        _ => return Err(Error::InvalidArgument),
    };
    let read = key(env, source.pointer, source.subresource);
    let write = key(env, destination.pointer, destination.subresource);
    execution(env.ext)?.append(
        Command::Transfer(transfer),
        extent_rect(destination.extent()),
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
    let backing = destination.acquire(env, false)?;
    let transfer = match (&backing, destination.resource) {
        (Backing::Buffer(buffer), Resource::Buffer(_)) => {
            let bytes = array::<u8>(env, data, size[0])?;
            let source = resource::upload(
                &execution(env.ext)?.gpu,
                &bytes,
                BufferUsage::COPY_SRC,
                u64::from(size[0]).next_multiple_of(4),
            )?;
            ModernTransfer::CopyBuffer {
                source: BufferRange {
                    buffer: source,
                    range: 0..u64::from(size[0]),
                },
                destination: buffer.clone(),
                offset: u64::from(origin[0]),
            }
        }
        (Backing::Image(image), Resource::Texture(texture)) => {
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
            let mut dest = resource::region(image, texture, destination.subresource);
            dest.origin = origin.into();
            dest.extent = Extent {
                width: size[0],
                height: size[1],
                depth_or_layers: size[2],
            };
            let source =
                resource::texture_transfer(&execution(env.ext)?.gpu, texture, &dest, &packed)?;
            ModernTransfer::Upload {
                source,
                destination: dest,
            }
        }
        _ => unreachable!(),
    };
    let write = key(env, destination.pointer, destination.subresource);
    execution(env.ext)?.append(
        Command::Transfer(transfer),
        extent_rect(extent),
        None,
        write,
    )
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
    Ok((backing.view(&view)?, key))
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
    let command = Command::Transfer(ModernTransfer::ClearColor {
        view,
        value: value.map(f64::from),
        area: None,
    });
    execution(env.ext)?.append(command, extent, None, key)
}

pub(super) fn clear_depth(env: &mut Env<'_>) -> Result<(), Error> {
    if null_device(env)? {
        return Ok(());
    }
    let (pointer, flags, depth, stencil) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let (view, key) = image_view(env, pointer, ViewKind::DepthStencil)?;
    let extent = view_rect(&view);
    let command = Command::Transfer(ModernTransfer::ClearDepth {
        view,
        depth: (flags & 1 != 0).then(|| f32::from_bits(depth).clamp(0., 1.)),
        stencil: (flags & 2 != 0).then_some(stencil & 255),
    });
    execution(env.ext)?.append(command, extent, None, key)
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
    let format = resource::native_format(env.arg(5)).ok_or(Error::InvalidArgument)?;
    let source_view = resolve_view(env, source, format)?;
    let destination = resolve_view(env, dest, format)?;
    let read = key(env, source.pointer, source.subresource);
    let write = key(env, dest.pointer, dest.subresource);
    let extent = view_rect(&destination);
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
    let (view, key) = image_view(env, env.arg(1), ViewKind::ShaderResource)?;
    let extent = view_rect(&view);
    execution(env.ext)?.append(Command::GenerateMips(view), extent, Some(key), key)
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
    let (command, extent) = match backing {
        Backing::Buffer(buffer) => {
            let Resource::Buffer(desc) = desc else {
                unreachable!()
            };
            let layout = crate::dxgi::formats::format_layout(view.format());
            let unit = layout.map_or(desc.stride().max(4), |format| {
                u32::from(format.aligned_unit_bytes)
            });
            if unit != 4 && view.format() != 0 {
                return Err(Error::InvalidArgument);
            }
            let range = view.elements();
            (
                ModernTransfer::FillBuffer {
                    destination: BufferRange {
                        buffer,
                        range: u64::from(range.start * unit)..u64::from(range.end * unit),
                    },
                    value: raw[0],
                },
                extent_rect([desc.size(), 1, 1]),
            )
        }
        Backing::Image(image) => {
            let view = Backing::Image(image).view(&view)?;
            let extent = view_rect(&view);
            let values = if integer {
                raw.map(f64::from)
            } else {
                raw.map(|value| f64::from(f32::from_bits(value)))
            };
            (
                ModernTransfer::ClearColor {
                    view,
                    value: values,
                    area: None,
                },
                extent,
            )
        }
    };
    execution(env.ext)?.append(Command::Transfer(command), extent, None, key)
}

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
) -> Result<(), Error> {
    let description = ops::object(env, table(env), pointer)?
        .resource()
        .ok_or(Error::InvalidArgument)?;
    let Backing::Image(mut image) = backing(env, pointer, description)? else {
        return Err(Error::InvalidArgument);
    };
    let key = key(env, pointer, 0);
    let size = (source[0], source[1]);
    let execution = execution(env.ext)?;
    let damage = execution
        .recording
        .present(&mut execution.gpu, key, size, serial, first)?;
    if image.desc().samples > 1 {
        let resolved = execution
            .gpu
            .device()
            .create_image(&adv_gpu::resource::ImageDesc {
                samples: 1,
                usage: adv_gpu::resource::Usage::COLOR
                    | adv_gpu::resource::Usage::SAMPLED
                    | adv_gpu::resource::Usage::COPY_SRC,
                ..*image.desc()
            })?;
        execution
            .gpu
            .resolve(image.view().clone(), resolved.view().clone())?;
        image = resolved;
    }
    if execution
        .present_target
        .as_ref()
        .is_none_or(|image| (image.width(), image.height()) != size)
    {
        execution.present_target = Some(
            execution
                .gpu
                .presentation_target(size.0, size.1, false)?
                .image,
        );
    }
    let source = adv_device::gpu::Image::from_native(image);
    let target = execution.present_target.as_ref().unwrap();
    let rect = extent_rect([size.0, size.1, 1]);
    execution.gpu.blit(
        &source,
        rect,
        target,
        rect,
        adv_device::gpu::Filter::Nearest,
    )?;
    execution.gpu.present(
        target,
        damage,
        serial,
        std::sync::Arc::new([]),
        std::sync::Arc::new([]),
    )?;
    Ok(())
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
    let execution = execution(env.ext)?;
    for rect in rectangles.into_iter().filter(|rect| !rect.empty()) {
        let writes = [(key, false)];
        let command = Command::Transfer(ModernTransfer::ClearColor {
            view: view.clone(),
            value,
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
    Ok(())
}

pub(super) fn check(ext: &Ext) -> Result<(), Error> {
    if let Some(execution) = ext.get::<State>().execution.as_ref() {
        execution.gpu.check()?;
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
    if initial == 0 || pointers.is_empty() || null_device(env)? {
        return Ok(());
    }
    let values = array::<u32>(env, initial, pointers.len() as u32)?;
    for (&pointer, value) in pointers.iter().zip(values) {
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
