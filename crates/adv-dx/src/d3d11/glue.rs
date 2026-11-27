//! Marshal generated device-child COM signatures onto the object table.
use super::{
    abi::*,
    desc::{PipelineState, Resource, TextureKind, View, ViewKind},
    object::{Error as ObjectError, Object, PrivateData, Release, Table, map},
    ops::{self, Error},
};
use adv_abi::{Api, Env, GuestPtr, ImplementationLevel, Outcome, Registry, com::Iid, signature};
use adv_cpu::backend::{Backend, GuestExt, Prot};
use adv_machine::Machine;
use bytemuck::Pod;
use context::list_flags;
use device::{query_desc, query_size};
use std::{collections::BTreeMap, num::NonZeroU32};

pub(super) mod context;
mod d3d10;
mod device;
mod dxgi;
#[cfg(feature = "gpu")]
mod gpu;
#[cfg(feature = "gpu")]
pub(super) use gpu::ShaderVariants;
#[cfg(all(feature = "gpu", feature = "d3d9-counters"))]
pub use gpu::{frame_counters, traffic, variant_counters};
mod linkage;

const DLL: &str = "d3d11.dll";
type Slot = (&'static str, adv_abi::Handler, ImplementationLevel);

macro_rules! face {
    ($interface:literal $(, $method:ident => $handler:ident $level:ident)* $(,)?) => {
        ($interface, &[
            (concat!($interface, "::QueryInterface"), query, ImplementationLevel::Implement),
            (concat!($interface, "::AddRef"), add_ref, ImplementationLevel::Implement),
            (concat!($interface, "::Release"), release, ImplementationLevel::Implement),
            (concat!($interface, "::GetDevice"), get_device, ImplementationLevel::Implement),
            (concat!($interface, "::GetPrivateData"), get_private, ImplementationLevel::Implement),
            (concat!($interface, "::SetPrivateData"), set_private, ImplementationLevel::Implement),
            (concat!($interface, "::SetPrivateDataInterface"), set_interface, ImplementationLevel::Implement),
            $((concat!($interface, "::", stringify!($method)), $handler, ImplementationLevel::$level),)*
        ])
    };
}

macro_rules! resource {
    ($interface:literal) => {
        face!($interface,
            GetType => get_type State,
            SetEvictionPriority => set_priority State,
            GetEvictionPriority => get_priority State,
            GetDesc => resource_desc State,
        )
    };
}

macro_rules! view {
    ($interface:literal) => {
        face!($interface, GetResource => get_resource Implement, GetDesc => view_desc State)
    };
}

use linkage::{class_desc, create_instance, get_instance, get_linkage, instance_name, type_name};

const FACES: &[(&str, &[Slot])] = &[
    device::FACE,
    device::MULTITHREAD,
    dxgi::FACE,
    dxgi::SURFACE,
    dxgi::RESOURCE,
    dxgi::KEYED_MUTEX,
    context::FACE,
    face!("ID3DDeviceContextState"),
    face!("ID3D11CommandList", GetContextFlags => list_flags State),
    face!("ID3D11VertexShader"),
    face!("ID3D11PixelShader"),
    face!("ID3D11ComputeShader"),
    face!("ID3D11GeometryShader"),
    face!("ID3D11HullShader"),
    face!("ID3D11DomainShader"),
    face!("ID3D11ClassLinkage", GetClassInstance => get_instance Implement, CreateClassInstance => create_instance Implement),
    face!("ID3D11ClassInstance", GetClassLinkage => get_linkage Implement, GetDesc => class_desc State, GetInstanceName => instance_name State, GetTypeName => type_name State),
    face!("ID3D11InputLayout"),
    face!("ID3D11Query", GetDataSize => query_size State, GetDesc => query_desc State),
    face!("ID3D11Predicate", GetDataSize => query_size State, GetDesc => query_desc State),
    resource!("ID3D11Buffer"),
    resource!("ID3D11Texture1D"),
    resource!("ID3D11Texture2D"),
    resource!("ID3D11Texture3D"),
    view!("ID3D11ShaderResourceView"),
    view!("ID3D11RenderTargetView"),
    view!("ID3D11DepthStencilView"),
    view!("ID3D11UnorderedAccessView"),
    face!("ID3D11BlendState1", GetDesc => pipeline_desc State, GetDesc1 => pipeline_desc1 State),
    face!("ID3D11DepthStencilState", GetDesc => pipeline_desc State),
    face!("ID3D11RasterizerState1", GetDesc => pipeline_desc State, GetDesc1 => pipeline_desc1 State),
    face!("ID3D11SamplerState", GetDesc => pipeline_desc State),
];

pub(super) struct State {
    table: Table,
    pub(super) layers: Vec<[u32; 4]>,
    devices: device::CreateDevices,
    force_d3d9: bool,
    #[cfg(feature = "gpu")]
    execution: Option<gpu::Execution>,
    #[cfg(feature = "gpu")]
    signal_event: fn(&mut Env<'_>, u32) -> bool,
    #[cfg(feature = "gpu")]
    shared: BTreeMap<u32, std::sync::Weak<super::object::Shared>>,
    #[cfg(feature = "gpu")]
    last_shared: u32,
    vtables: BTreeMap<&'static str, u32>,
}

pub fn register_children(registry: &mut Registry) {
    super::exports::register(registry, device::create_core);
    registry.register(Api {
        module: DLL,
        name: "D3D11CreateDevice",
        handler: device::create,
        level: ImplementationLevel::Implement,
        conv: signature::lookup(DLL, "D3D11CreateDevice")
            .unwrap()
            .conv
            .unwrap(),
    });
    registry.register(Api {
        module: DLL,
        name: "D3D11CreateDeviceAndSwapChain",
        handler: device::create_and_swap,
        level: ImplementationLevel::Implement,
        conv: signature::lookup(DLL, "D3D11CreateDeviceAndSwapChain")
            .unwrap()
            .conv
            .unwrap(),
    });
    for &(face, slots) in FACES.iter().chain(d3d10::FACES) {
        for (index, &(name, handler, level)) in slots.iter().enumerate() {
            let (_, method) = name.split_once("::").unwrap();
            let signature =
                signature::lookup_com(face, method).expect("generated D3D11 COM signature");
            assert_eq!(signature.com_slot, Some(index as u16));
            registry.register(Api {
                module: DLL,
                name,
                handler,
                level,
                conv: signature.conv.expect("COM convention"),
            });
        }
    }
}

pub fn install_children<B: Backend>(machine: &mut Machine<B>) -> Result<(), String> {
    let table = Table::new(&mut machine.sys.objects).map_err(|_| "D3D11 COM namespace")?;
    let size = FACES
        .iter()
        .chain(d3d10::FACES)
        .map(|(_, slots)| slots.len())
        .sum::<usize>() as u32
        * 4;
    let mut address = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, size, Prot::RW)
        .map_err(|e| e.to_string())?;
    let mut vtables = BTreeMap::new();
    for &(face, slots) in FACES.iter().chain(d3d10::FACES) {
        vtables.insert(face, address);
        for &(name, _, _) in slots {
            let entry = machine
                .sys
                .thunks
                .publish(&mut machine.cpu, DLL, name)
                .map_err(|e| e.to_string())?;
            machine
                .cpu
                .write_u32(address, entry)
                .map_err(|e| e.to_string())?;
            address += 4;
        }
    }
    machine.ext.insert(State {
        table,
        layers: Vec::new(),
        vtables,
        devices: Default::default(),
        force_d3d9: std::env::var_os("ADV_FORCE_D3D9").is_some_and(|value| value == "1"),
        #[cfg(feature = "gpu")]
        execution: None,
        #[cfg(feature = "gpu")]
        signal_event: |env, event| env.sys.set_event(event),
        #[cfg(feature = "gpu")]
        shared: BTreeMap::new(),
        #[cfg(feature = "gpu")]
        last_shared: 0xf000_0000,
    });
    Ok(())
}

fn table(env: &Env<'_>) -> Table {
    env.ext.get::<State>().table
}

fn alloc(env: &mut Env<'_>, face: &str) -> Result<u32, ObjectError> {
    let vtable = env.ext.get::<State>().vtables[face];
    let heaps = env.ext.get_mut::<adv_os::Heaps>();
    let pointer = heaps
        .alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, 4, false)
        .ok_or(ObjectError::Exhausted)?;
    if env.cpu.write_u32(pointer, vtable).is_err() {
        free(env, pointer);
        return Err(ObjectError::Exhausted);
    }
    Ok(pointer)
}

fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<adv_os::Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}

pub fn attach_resource(
    env: &mut Env<'_>,
    device: u32,
    resource: Resource,
) -> Result<u32, ObjectError> {
    let face = match resource {
        Resource::Buffer(_) => "ID3D11Buffer",
        Resource::Texture(texture) => match texture.kind() {
            TextureKind::D1 => "ID3D11Texture1D",
            TextureKind::D2 => "ID3D11Texture2D",
            TextureKind::D3 => "ID3D11Texture3D",
        },
    };
    let layouts = if map::has_shadow(resource) {
        let alignment = NonZeroU32::new(128).unwrap();
        Some(map::Layouts::new(resource, alignment).ok_or(ObjectError::Exhausted)?)
    } else {
        None
    };
    let pointer = alloc(env, face)?;
    let (storage, mapping) = match layouts {
        Some(layouts) => {
            let heaps = env.ext.get_mut::<adv_os::Heaps>();
            let Some(storage) = heaps.alloc(
                env.cpu,
                &mut env.sys.vm,
                heaps.process_heap,
                layouts.size(),
                true,
            ) else {
                free(env, pointer);
                return Err(ObjectError::Exhausted);
            };
            (Some(storage), Some(layouts.attach(storage)))
        }
        None => (None, None),
    };
    let result =
        table(env).attach_resource(&mut env.sys.objects, pointer, device, resource, mapping);
    if result.is_err() {
        if let Some(storage) = storage {
            free(env, storage);
        }
        free(env, pointer);
    }
    result?;
    if let Err(error) = dxgi::attach_resource_face(env, pointer) {
        let released = table(env).release(&mut env.sys.objects, pointer);
        retire(env, released);
        return Err(error);
    }
    if let Resource::Texture(texture) = resource
        && texture.kind() == TextureKind::D2
        && texture.levels() == 1
        && texture.layers() == 1
        && let Err(error) = dxgi::attach_surface_face(env, pointer)
    {
        let table = table(env);
        let release = table.release(&mut env.sys.objects, pointer);
        retire(env, release);
        return Err(error);
    }
    if let Err(error) = d3d10::attach_child(env, pointer) {
        let release = table(env).release(&mut env.sys.objects, pointer);
        retire(env, release);
        return Err(error);
    }
    Ok(pointer)
}

pub fn attach_view(env: &mut Env<'_>, resource: u32, view: View) -> Result<u32, ObjectError> {
    let face = match view.kind() {
        ViewKind::ShaderResource => "ID3D11ShaderResourceView",
        ViewKind::RenderTarget => "ID3D11RenderTargetView",
        ViewKind::DepthStencil => "ID3D11DepthStencilView",
        ViewKind::UnorderedAccess => "ID3D11UnorderedAccessView",
    };
    let pointer = alloc(env, face)?;
    let result = table(env).attach_view(&mut env.sys.objects, pointer, resource, view);
    if result.is_err() {
        free(env, pointer);
    }
    result.map(|()| pointer)
}

pub fn attach_pipeline(
    env: &mut Env<'_>,
    device: u32,
    pipeline: PipelineState,
) -> Result<u32, ObjectError> {
    let table = table(env);
    if let Some(existing) = table.find_pipeline(&mut env.sys.objects, device, &pipeline)? {
        return Ok(existing);
    }
    let face = match pipeline {
        PipelineState::Blend(_) => "ID3D11BlendState1",
        PipelineState::DepthStencil(_) => "ID3D11DepthStencilState",
        PipelineState::Rasterizer(_) => "ID3D11RasterizerState1",
        PipelineState::Sampler(_) => "ID3D11SamplerState",
    };
    let pointer = alloc(env, face)?;
    let result = table.attach_pipeline(&mut env.sys.objects, pointer, device, pipeline);
    if result.is_err() {
        free(env, pointer);
    }
    result
}

impl Error {
    fn hresult(self) -> u32 {
        match self {
            #[cfg(feature = "gpu")]
            Self::InitialDiscardRequired => 0x887c0004,
            Self::InvalidArgument | Self::Object(ObjectError::InvalidObject) => 0x80070057,
            Self::NotFound => 0x887a0002,
            Self::MoreData => 0x887a0003,
            Self::Caps(error) => error.hresult(),
            Self::Description(super::desc::Error::InvalidArgument) => 0x80070057,
            Self::Description(super::desc::Error::UnsupportedView) => 0x80004005,
            Self::Dxgi(error) => error.hresult(),
            Self::Map(map::Error::InvalidArgument) => 0x80070057,
            Self::Map(map::Error::StillDrawing) => 0x887a000a,
            #[cfg(feature = "gpu")]
            Self::Device => 0x887a0005,
            Self::Object(ObjectError::NoInterface) => 0x80004002,
            Self::Object(ObjectError::Exhausted) => 0x8007000e,
            Self::Object(ObjectError::TooManyUniqueStates) => 0x887c0001,
        }
    }
}

fn call(result: Result<(), Error>) -> Outcome {
    Outcome::Return(result.map_or_else(Error::hresult, |()| 0))
}

pub(super) fn read<T: Pod>(env: &Env<'_>, addr: u32) -> Result<T, Error> {
    GuestPtr::new(addr)
        .read(env.cpu)
        .map_err(|_| Error::InvalidArgument)
}

pub(super) fn write<T: Pod>(env: &mut Env<'_>, addr: u32, value: T) -> Result<(), Error> {
    GuestPtr::new(addr)
        .write(env.cpu, value)
        .map_err(|_| Error::InvalidArgument)
}

fn query(env: &mut Env<'_>) -> Outcome {
    let (this, iid, out) = (env.arg(0), read(env, env.arg(1)), env.arg(2));
    let result = (|| {
        write(env, out, 0u32)?;
        let pointer = ops::query(env, table(env), this, iid?)?;
        write(env, out, pointer)
    })();
    call(result)
}

fn add_ref(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(ops::add_ref(env, table(env), env.arg(0)).unwrap_or(0))
}

fn release(env: &mut Env<'_>) -> Outcome {
    let released = ops::release(env, table(env), env.arg(0));
    retire(env, released)
}

fn retire(env: &mut Env<'_>, released: Release) -> Outcome {
    #[cfg(feature = "gpu")]
    for event in released.keyed_wakes {
        env.sys.set_event(event);
    }
    #[cfg(feature = "gpu")]
    for handle in released.shared {
        env.ext.get_mut::<State>().shared.remove(&handle);
    }
    for pointer in released.allocations {
        free(env, pointer);
    }
    drop_private(env, released.private_interfaces, released.references)
}

fn drop_private(env: &mut Env<'_>, mut interfaces: Vec<u32>, status: u32) -> Outcome {
    match interfaces.pop() {
        Some(pointer) => unknown(env, pointer, 2, move |env| {
            drop_private(env, interfaces, status)
        }),
        None => Outcome::Return(status),
    }
}

fn unknown(
    env: &mut Env<'_>,
    pointer: u32,
    slot: u32,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let Ok(target) = env
        .cpu
        .read_u32(pointer)
        .and_then(|vtable| env.cpu.read_u32(vtable + slot * 4))
    else {
        return Outcome::Return(Error::InvalidArgument.hresult());
    };
    Outcome::call(target, [pointer], move |env, _| then(env))
}

fn get_resource(env: &mut Env<'_>) -> Outcome {
    related(env, |obj| obj.view().map(|(resource, _)| resource))
}

fn get_device(env: &mut Env<'_>) -> Outcome {
    related(env, Object::device)
}

fn related(env: &mut Env<'_>, relation: impl FnOnce(&Object) -> Option<u32>) -> Outcome {
    let (this, output) = (env.arg(0), env.arg(1));
    let result = (|| {
        write(env, output, 0u32)?;
        let pointer = ops::related(env, table(env), this, relation)?;
        write(env, output, pointer)
    })();
    call(result)
}

fn set_private(env: &mut Env<'_>) -> Outcome {
    let (this, key, len, data) = (env.arg(0), read(env, env.arg(1)), env.arg(2), env.arg(3));
    let value = (|| {
        let key = key?;
        if len == 0 {
            return Ok((key, None));
        }
        adv_abi::guest::check_range(env.cpu, data, len, Prot::READ)
            .map_err(|_| Error::InvalidArgument)?;
        let mut bytes = vec![0; len as usize];
        env.cpu
            .read(data, &mut bytes)
            .map_err(|_| Error::InvalidArgument)?;
        Ok((key, Some(PrivateData::Bytes(bytes))))
    })();
    match value {
        Ok((key, data)) => store_private(env, this, key, data),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn set_interface(env: &mut Env<'_>) -> Outcome {
    let (this, key, interface) = (env.arg(0), read(env, env.arg(1)), env.arg(2));
    let key: Iid = match key {
        Ok(key) => key,
        Err(error) => return Outcome::Return(error.hresult()),
    };
    if interface == 0 {
        return store_private(env, this, key, Some(PrivateData::Interface(0)));
    }
    if let Err(error) = ops::object(env, table(env), this) {
        return Outcome::Return(error.hresult());
    }
    unknown(env, interface, 1, move |env| {
        store_private(env, this, key, Some(PrivateData::Interface(interface)))
    })
}

fn store_private(env: &mut Env<'_>, this: u32, key: Iid, value: Option<PrivateData>) -> Outcome {
    let remove = value.is_none();
    let acquired = match value {
        Some(PrivateData::Interface(pointer)) => pointer,
        _ => 0,
    };
    match ops::set_private(env, table(env), this, key, value) {
        Ok(Some(PrivateData::Interface(pointer))) if pointer != 0 => {
            unknown(env, pointer, 2, |_| Outcome::Return(0))
        }
        Ok(None) if remove => Outcome::Return(1),
        Ok(_) => Outcome::Return(0),
        Err(error) if acquired != 0 => {
            unknown(env, acquired, 2, move |_| Outcome::Return(error.hresult()))
        }
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_private(env: &mut Env<'_>) -> Outcome {
    let (this, key, length, data) = (
        env.arg(0),
        read::<Iid>(env, env.arg(1)),
        env.arg(2),
        env.arg(3),
    );
    let result = (|| {
        let key = key?;
        let available: u32 = read(env, length)?;
        let object = ops::object(env, table(env), this)?;
        let Some(value) = object.private(key) else {
            write(env, length, 0u32)?;
            return Err(Error::NotFound);
        };
        let (bytes, interface) = match value {
            PrivateData::Bytes(bytes) => (bytes.clone(), 0),
            PrivateData::Interface(pointer) => (pointer.to_le_bytes().to_vec(), *pointer),
        };
        write(env, length, bytes.len() as u32)?;
        if data != 0 {
            if (available as usize) < bytes.len() {
                return Err(Error::MoreData);
            }
            env.cpu
                .write(data, &bytes)
                .map_err(|_| Error::InvalidArgument)?;
            return Ok(interface);
        }
        Ok(0)
    })();
    match result {
        Ok(pointer) if pointer != 0 => unknown(env, pointer, 1, |_| Outcome::Return(0)),
        Ok(_) => Outcome::Return(0),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_type(env: &mut Env<'_>) -> Outcome {
    let (this, dest) = (env.arg(0), env.arg(1));
    let result = ops::object(env, table(env), this)
        .and_then(|obj| obj.resource().ok_or(Error::InvalidArgument))
        .and_then(|resource| write(env, dest, resource.dimension()));
    call(result)
}

fn get_priority(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(
        ops::object(env, table(env), env.arg(0))
            .ok()
            .and_then(Object::priority)
            .unwrap_or(0),
    )
}

fn set_priority(env: &mut Env<'_>) -> Outcome {
    let (this, value) = (env.arg(0), env.arg(1));
    if let Ok(obj) = ops::object_mut(env, table(env), this) {
        obj.set_priority(value);
    }
    Outcome::Return(0)
}

fn resource_desc(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    let result = (|| {
        let resource = ops::object(env, table(env), this)?
            .resource()
            .ok_or(Error::InvalidArgument)?;
        match resource {
            Resource::Buffer(buffer) => write(env, out, buffer.desc()),
            Resource::Texture(texture) => match texture.kind() {
                TextureKind::D1 => write(env, out, texture.desc_1d()),
                TextureKind::D2 => write(env, out, texture.desc_2d()),
                TextureKind::D3 => write(env, out, texture.desc_3d()),
            },
        }
    })();
    call(result)
}

fn view_desc(env: &mut Env<'_>) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    let result = (|| {
        let (_, view) = ops::object(env, table(env), this)?
            .view()
            .ok_or(Error::InvalidArgument)?;
        let view = view.clone();
        match view.kind() {
            ViewKind::ShaderResource => write(env, out, view.srv_desc()),
            ViewKind::RenderTarget => write(env, out, view.rtv_desc()),
            ViewKind::DepthStencil => write(env, out, view.dsv_desc()),
            ViewKind::UnorderedAccess => write(env, out, view.uav_desc()),
        }
    })();
    call(result)
}

fn pipeline_desc(env: &mut Env<'_>) -> Outcome {
    pipeline_description(env, false)
}

fn pipeline_desc1(env: &mut Env<'_>) -> Outcome {
    pipeline_description(env, true)
}

fn pipeline_description(env: &mut Env<'_>, version1: bool) -> Outcome {
    let (this, out) = (env.arg(0), env.arg(1));
    let result = (|| {
        let pipeline = ops::object(env, table(env), this)?
            .pipeline()
            .cloned()
            .ok_or(Error::InvalidArgument)?;
        match pipeline {
            PipelineState::Blend(desc) if version1 => write(env, out, *desc),
            PipelineState::Blend(desc) => write(
                env,
                out,
                D3D11_BLEND_DESC {
                    AlphaToCoverageEnable: desc.AlphaToCoverageEnable,
                    IndependentBlendEnable: desc.IndependentBlendEnable,
                    RenderTarget: desc
                        .RenderTarget
                        .map(|target| D3D11_RENDER_TARGET_BLEND_DESC {
                            BlendEnable: target.BlendEnable,
                            SrcBlend: target.SrcBlend,
                            DestBlend: target.DestBlend,
                            BlendOp: target.BlendOp,
                            SrcBlendAlpha: target.SrcBlendAlpha,
                            DestBlendAlpha: target.DestBlendAlpha,
                            BlendOpAlpha: target.BlendOpAlpha,
                            RenderTargetWriteMask: target.RenderTargetWriteMask,
                            _padding0: [0; 3],
                        }),
                },
            ),
            PipelineState::Rasterizer(desc) if version1 => write(env, out, desc),
            PipelineState::Rasterizer(desc) => write(
                env,
                out,
                bytemuck::pod_read_unaligned::<D3D11_RASTERIZER_DESC>(
                    &bytemuck::bytes_of(&desc)[..size_of::<D3D11_RASTERIZER_DESC>()],
                ),
            ),
            PipelineState::DepthStencil(desc) => write(env, out, desc),
            PipelineState::Sampler(desc) => write(env, out, desc),
        }
    })();
    call(result)
}

#[cfg(test)]
mod tests;

pub fn configure_devices<B: Backend>(
    machine: &mut Machine<B>,
    hardware: &adv_gpu::Caps,
    paths: super::caps::Coverage<'_>,
) {
    machine.ext.get_mut::<State>().devices = device::CreateDevices::derive(hardware, paths);
}

pub(super) fn array<T: Pod>(env: &Env<'_>, address: u32, count: u32) -> Result<Vec<T>, Error> {
    let bytes = count
        .checked_mul(size_of::<T>() as u32)
        .ok_or(Error::InvalidArgument)?;
    if bytes == 0 {
        return Ok(Vec::new());
    }
    adv_abi::guest::check_range(env.cpu, address, bytes, Prot::READ)
        .map_err(|_| Error::InvalidArgument)?;
    let mut values = vec![T::zeroed(); count as usize];
    env.cpu
        .read(address, bytemuck::cast_slice_mut(&mut values))
        .map_err(|_| Error::InvalidArgument)?;
    Ok(values)
}

fn output(
    env: &mut Env<'_>,
    address: u32,
    make: impl FnOnce(&mut Env<'_>) -> Result<u32, Error>,
) -> Result<u32, Error> {
    if address == 0 {
        return Ok(1);
    }
    write(env, address, 0u32)?;
    let pointer = make(env)?;
    if let Err(error) = d3d10::attach_child(env, pointer) {
        let release = table(env).release(&mut env.sys.objects, pointer);
        retire(env, release);
        return Err(error.into());
    }
    write(env, address, pointer)?;
    Ok(0)
}

fn status(result: Result<u32, Error>) -> Outcome {
    Outcome::Return(result.unwrap_or_else(Error::hresult))
}

#[cfg(feature = "gpu")]
pub fn configure_gpu<B: Backend>(
    machine: &mut Machine<B>,
    gpu: adv_device::gpu::Gpu,
    paths: super::caps::Coverage<'_>,
    wake: adv_sync::WakeEvent,
) {
    let caps = gpu.device().caps();
    crate::dxgi::set_adapter(
        &mut machine.ext,
        crate::dxgi::caps::AdapterInfo::from_gpu(caps),
    );
    let production = super::caps::production_formats(caps);
    let formats = production.iter().map(|row| row.path()).collect::<Vec<_>>();
    configure_devices(
        machine,
        caps,
        super::caps::Coverage {
            formats: &formats,
            ..paths
        },
    );
    let mut execution = gpu::Execution::new(gpu);
    execution.wake = Some(wake);
    machine.ext.get_mut::<State>().execution = Some(execution);
}

#[cfg(feature = "gpu")]
pub fn fork_gpu<B: Backend>(
    parent: &adv_machine::Ext,
    child: &mut Machine<B>,
    wake: adv_sync::WakeEvent,
) -> Result<(), String> {
    if let Some(execution) = &parent.get::<State>().execution {
        let gpu = execution.fork().map_err(|error| error.to_string())?;
        configure_gpu(child, gpu, super::caps::Coverage::production(), wake);
    }
    Ok(())
}

#[cfg(feature = "gpu")]
pub fn pump<B: Backend>(machine: &mut Machine<B>) -> Result<bool, String> {
    let state = machine.ext.get_mut::<State>();
    let Some(execution) = &mut state.execution else {
        return Ok(false);
    };
    execution.poll().map_err(|error| error.to_string())?;
    let ready = execution.completed_events();
    let mut progress = execution.wake_presents(&mut machine.sys);
    let signal = state.signal_event;
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for event in ready {
        progress |= signal(&mut env, event);
    }
    Ok(progress)
}

#[cfg(feature = "gpu")]
pub fn configure_event_signal<B: Backend>(
    machine: &mut Machine<B>,
    signal: fn(&mut Env<'_>, u32) -> bool,
) {
    machine.ext.get_mut::<State>().signal_event = signal;
}

#[cfg(feature = "gpu")]
pub fn configure_swap_chains<B: Backend>(
    machine: &mut Machine<B>,
    reported_build: u32,
    window: fn(&adv_machine::Ext, &adv_machine::Objects, u32) -> Option<crate::dxgi::swap::Window>,
    resize: crate::dxgi::swap::ResizeWindow,
    completed: Option<crate::dxgi::swap::FrameCompleted>,
) {
    crate::dxgi::install_swap_chains(
        machine,
        crate::dxgi::swap::Services {
            buffers: &dxgi::SwapBuffers,
            window,
            resize,
            completed,
            reported_build,
        },
    );
}

#[cfg(feature = "gpu")]
pub fn configure_surface_dc<B: Backend>(
    machine: &mut Machine<B>,
    provider: crate::interop::MemoryDc,
) {
    machine.ext.insert(provider);
}
