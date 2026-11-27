//! Marshal generated x86 signatures onto the factory, adapter and output ops.
use super::Error;
use super::abi::*;
use super::caps::{ADAPTERS, Adapter};
use super::desc::{self, ModeRequest};
use super::object::{Notification, PrivateData};
use super::ops::{self, FactoryOrigin};
use super::state::{DeviceCall, DeviceOperation, Provider, State};
use adv_abi::{
    Api, Env, GuestPtr, ImplementationLevel, Outcome, Registry, com::Iid, signature, win32,
};
use adv_cpu::backend::{Backend, Prot};
use adv_machine::Machine;
use bytemuck::Pod;

const DLL: &str = "dxgi.dll";
type Slot = (&'static str, adv_abi::Handler, ImplementationLevel);

macro_rules! vtable {
    ($face:literal, $($method:ident => $handler:ident $level:ident),+ $(,)?) => {
        &[$((concat!($face, "::", stringify!($method)), $handler, ImplementationLevel::$level)),+]
    };
}

mod exports;
pub(super) mod output;
mod swap_chain;

const FACTORY: &[Slot] = vtable!("IDXGIFactory7",
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    EnumAdapters => enum_adapters Implement,
    MakeWindowAssociation => associate_window State,
    GetWindowAssociation => window State,
    CreateSwapChain => create_swap Implement,
    CreateSoftwareAdapter => software_adapter SafeFail,
    EnumAdapters1 => enum_adapters Implement,
    IsCurrent => yes State,
    IsWindowedStereoEnabled => zero State,
    CreateSwapChainForHwnd => create_swap_hwnd Implement,
    CreateSwapChainForCoreWindow => create_swap_core Implement,
    GetSharedResourceAdapterLuid => shared_resource_luid Implement,
    RegisterStereoStatusWindow => register_stereo_window State,
    RegisterStereoStatusEvent => register_stereo_event State,
    UnregisterStereoStatus => unregister_stereo State,
    RegisterOcclusionStatusWindow => register_occlusion_window State,
    RegisterOcclusionStatusEvent => register_occlusion_event State,
    UnregisterOcclusionStatus => unregister_occlusion State,
    CreateSwapChainForComposition => create_swap_composition Implement,
    GetCreationFlags => zero State,
    EnumAdapterByLuid => enum_adapter_by_luid Implement,
    EnumWarpAdapter => enum_warp Implement,
    CheckFeatureSupport => factory_feature Implement,
    EnumAdapterByGpuPreference => enum_adapter_by_preference Implement,
    RegisterAdaptersChangedEvent => register_adapters State,
    UnregisterAdaptersChangedEvent => unregister_adapters State,
);

const ADAPTER: &[Slot] = vtable!("IDXGIAdapter4",
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    EnumOutputs => enum_outputs Implement,
    GetDesc => adapter_desc Implement,
    CheckInterfaceSupport => check_interface Implement,
    GetDesc1 => adapter_desc1 Implement,
    GetDesc2 => adapter_desc2 Implement,
    RegisterHardwareContentProtectionTeardownStatusEvent => register_content_protection State,
    UnregisterHardwareContentProtectionTeardownStatus => unregister_content_protection State,
    QueryVideoMemoryInfo => video_memory Implement,
    SetVideoMemoryReservation => reserve_memory Implement,
    RegisterVideoMemoryBudgetChangeNotificationEvent => register_budget State,
    UnregisterVideoMemoryBudgetChangeNotification => unregister_budget State,
    GetDesc3 => adapter_desc3 Implement,
);

const OUTPUT: &[Slot] = vtable!("IDXGIOutput6",
    QueryInterface => query Implement,
    AddRef => add_ref Implement,
    Release => release Implement,
    SetPrivateData => set_private Implement,
    SetPrivateDataInterface => set_interface Implement,
    GetPrivateData => get_private Implement,
    GetParent => parent Implement,
    GetDesc => output_desc Implement,
    GetDisplayModeList => mode_list Implement,
    FindClosestMatchingMode => closest_mode Implement,
    WaitForVBlank => wait_vblank Implement,
    TakeOwnership => take_ownership Implement,
    ReleaseOwnership => release_ownership Implement,
    GetGammaControlCapabilities => gamma_caps Implement,
    SetGammaControl => set_gamma Implement,
    GetGammaControl => get_gamma Implement,
    SetDisplaySurface => set_surface Implement,
    GetDisplaySurfaceData => get_surface Implement,
    GetFrameStatistics => frame_statistics Implement,
    GetDisplayModeList1 => mode_list1 Implement,
    FindClosestMatchingMode1 => closest_mode1 Implement,
    GetDisplaySurfaceData1 => get_surface1 Implement,
    DuplicateOutput => duplicate Implement,
    SupportsOverlays => zero State,
    CheckOverlaySupport => overlay Implement,
    CheckOverlayColorSpaceSupport => overlay_color_space Implement,
    DuplicateOutput1 => duplicate1 Implement,
    GetDesc1 => output_desc1 Implement,
    CheckHardwareCompositionSupport => composition_support Implement,
);

pub fn register(registry: &mut Registry) {
    exports::register(registry);
    registry.register(create_factory_ABI);
    registry.register(create_factory1_ABI);
    for slots in [FACTORY, ADAPTER, OUTPUT, swap_chain::SLOTS, output::SLOTS] {
        for (index, &(name, handler, level)) in slots.iter().enumerate() {
            let (interface, method) = name.split_once("::").unwrap();
            let signature =
                signature::lookup_com(interface, method).expect("generated DXGI COM signature");
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

pub fn install<B: Backend>(machine: &mut Machine<B>, provider: Provider) -> Result<(), String> {
    let table = |machine: &mut Machine<B>, slots: &[Slot]| {
        let mut entries = Vec::with_capacity(slots.len());
        for &(name, _, _) in slots {
            entries.push(
                machine
                    .sys
                    .thunks
                    .publish(&mut machine.cpu, DLL, name)
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok::<_, String>(entries)
    };
    let entries = [
        table(machine, FACTORY)?,
        table(machine, ADAPTER)?,
        table(machine, OUTPUT)?,
        table(machine, swap_chain::SLOTS)?,
        table(machine, output::SLOTS)?,
    ];
    let com = adv_abi::com::Com::new(&mut machine.sys.objects).ok_or("DXGI COM namespace")?;
    let storage = machine
        .sys
        .vm
        .alloc(
            &mut machine.cpu,
            entries.iter().map(|v| v.len() as u32 * 4).sum(),
            Prot::RW,
        )
        .map_err(|e| e.to_string())?;
    let mut address = storage;
    let [
        factory_vtable,
        adapter_vtable,
        output_vtable,
        swap_chain_vtable,
        duplication_vtable,
    ] = entries.each_ref().map(|entries| {
        let start = address;
        address += entries.len() as u32 * 4;
        start
    });
    for (address, entries) in [
        factory_vtable,
        adapter_vtable,
        output_vtable,
        swap_chain_vtable,
        duplication_vtable,
    ]
    .into_iter()
    .zip(&entries)
    {
        machine
            .cpu
            .write(address, bytemuck::cast_slice(entries))
            .map_err(|e| e.to_string())?;
    }
    machine.ext.insert(State {
        com,
        factory_vtable,
        adapter_vtable,
        output_vtable,
        swap_chain_vtable,
        duplication_vtable,
        provider,
        presentation: 0,
        output: Default::default(),
        chains: Default::default(),
    });
    Ok(())
}

/// Construct the parent factory for a device-created adapter.
pub fn create_device_factory(env: &mut Env<'_>, iid: Iid) -> Result<u32, u32> {
    ops::create_factory(env, FactoryOrigin::Device, iid).map_err(Error::hresult)
}

pub fn query_adapter(env: &mut Env<'_>, adapter: u32, iid: Iid) -> Result<u32, Error> {
    ops::query(env, adapter, iid)
}

pub fn adapter_kind(env: &Env<'_>, adapter: u32) -> Result<Adapter, Error> {
    ops::adapter(env, adapter)
}

pub fn create_device_adapter(env: &mut Env<'_>, adapter: Adapter) -> Result<u32, Error> {
    let factory = ops::create_factory(env, FactoryOrigin::Device, IID_IDXGIFACTORY1)?;
    let result = ops::enum_adapter(env, factory, adapter);
    ops::release(env, factory);
    result
}

fn call(f: impl FnOnce() -> Result<(), Error>) -> Outcome {
    Outcome::Return(f().map_or_else(Error::hresult, |()| 0))
}

fn read<T: Pod>(env: &Env<'_>, address: u32) -> Result<T, Error> {
    GuestPtr::new(address)
        .read(env.cpu)
        .map_err(|_| Error::InvalidCall)
}

fn write<T: Pod>(env: &mut Env<'_>, address: u32, value: T) -> Result<(), Error> {
    GuestPtr::new(address)
        .write(env.cpu, value)
        .map_err(|_| Error::InvalidCall)
}

fn interface_out(
    env: &mut Env<'_>,
    output: u32,
    f: impl FnOnce(&mut Env<'_>) -> Result<u32, Error>,
) -> Outcome {
    call(|| {
        write(env, output, 0u32)?;
        let pointer = f(env)?;
        write(env, output, pointer)
    })
}

#[win32(dxgi, name = "CreateDXGIFactory", level = implement)]
fn create_factory(env: &mut Env<'_>, iid: GuestPtr<Iid>, output: GuestPtr<u32>) -> Outcome {
    create_factory_from(env, iid, output, FactoryOrigin::Legacy)
}

#[win32(dxgi, name = "CreateDXGIFactory1", level = implement)]
fn create_factory1(env: &mut Env<'_>, iid: GuestPtr<Iid>, output: GuestPtr<u32>) -> Outcome {
    create_factory_from(env, iid, output, FactoryOrigin::Version1)
}

fn create_factory_from(
    env: &mut Env<'_>,
    iid: GuestPtr<Iid>,
    output: GuestPtr<u32>,
    origin: FactoryOrigin,
) -> Outcome {
    let iid = read(env, iid.addr());
    interface_out(env, output.addr(), |env| {
        ops::create_factory(env, origin, iid?)
    })
}

fn query(env: &mut Env<'_>) -> Outcome {
    let (this, iid, output) = (env.arg(0), read(env, env.arg(1)), env.arg(2));
    interface_out(env, output, |env| ops::query(env, this, iid?))
}

fn parent(env: &mut Env<'_>) -> Outcome {
    let (this, iid, output) = (env.arg(0), read(env, env.arg(1)), env.arg(2));
    interface_out(env, output, |env| ops::parent(env, this, iid?))
}

fn add_ref(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(ops::add_ref(env, env.arg(0)))
}

fn release(env: &mut Env<'_>) -> Outcome {
    ops::release(env, env.arg(0))
}

fn set_private(env: &mut Env<'_>) -> Outcome {
    let (this, name, size, data) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let parsed = || {
        let iid = read(env, name)?;
        if data == 0 {
            return Ok((iid, None));
        }
        adv_abi::guest::check_range(env.cpu, data, size, Prot::READ)
            .map_err(|_| Error::InvalidCall)?;
        let mut bytes = vec![0; size as usize];
        env.cpu
            .read(data, &mut bytes)
            .map_err(|_| Error::InvalidCall)?;
        Ok((iid, Some(PrivateData::Bytes(bytes))))
    };
    match parsed() {
        Ok((iid, data)) => ops::store_private(env, this, iid, data),
        Err(error) => Outcome::Return(Error::hresult(error)),
    }
}

fn set_interface(env: &mut Env<'_>) -> Outcome {
    let (this, name, pointer) = (env.arg(0), env.arg(1), env.arg(2));
    match read(env, name) {
        Ok(iid) => ops::store_interface(env, this, iid, pointer),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn get_private(env: &mut Env<'_>) -> Outcome {
    let (this, iid, size, output) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let mut copy = || {
        let value = ops::private(env, this, read(env, iid)?)?;
        let capacity: u32 = read(env, size)?;
        let length = match &value {
            PrivateData::Bytes(bytes) => bytes.len() as u32,
            PrivateData::Interface(_) => 4,
        };
        write(env, size, length)?;
        if output == 0 {
            return Ok(None);
        }
        if capacity < length {
            return Err(Error::MoreData);
        }
        validate_output(env, output, length)?;
        match value {
            PrivateData::Bytes(bytes) => {
                env.cpu
                    .write(output, &bytes)
                    .map_err(|_| Error::InvalidCall)?;
                Ok(None)
            }
            PrivateData::Interface(pointer) => {
                write(env, output, pointer)?;
                Ok(Some(pointer))
            }
        }
    };
    match copy() {
        Ok(Some(pointer)) => ops::retain_private(env, pointer),
        Ok(None) => Outcome::Return(0),
        Err(error) => Outcome::Return(error.hresult()),
    }
}

fn enum_adapters(env: &mut Env<'_>) -> Outcome {
    let (this, index, output) = (env.arg(0), env.arg(1), env.arg(2));
    interface_out(env, output, |env| {
        let adapter = ADAPTERS
            .get(index as usize)
            .copied()
            .ok_or(Error::NotFound)?;
        ops::enum_adapter(env, this, adapter)
    })
}

fn enum_adapter_iid(
    env: &mut Env<'_>,
    adapter: Result<Adapter, Error>,
    iid: u32,
    output: u32,
) -> Outcome {
    let this = env.arg(0);
    interface_out(env, output, |env| {
        let iid: Iid = read(env, iid)?;
        if !ops::ADAPTER_INTERFACES.contains(&iid)
            && !matches!(iid, IID_IDXGIOBJECT | adv_abi::com::IUNKNOWN)
        {
            return Err(Error::NoInterface);
        }
        ops::enum_adapter(env, this, adapter?)
    })
}

fn enum_warp(env: &mut Env<'_>) -> Outcome {
    enum_adapter_iid(env, Ok(Adapter::Software), env.arg(1), env.arg(2))
}

fn enum_adapter_by_luid(env: &mut Env<'_>) -> Outcome {
    let luid = u64::from(env.arg(1)) | (u64::from(env.arg(2)) << 32);
    enum_adapter_iid(env, ops::adapter_by_luid(env, luid), env.arg(3), env.arg(4))
}

fn enum_adapter_by_preference(env: &mut Env<'_>) -> Outcome {
    let adapter = match env.arg(2) {
        0..=2 => ADAPTERS
            .get(env.arg(1) as usize)
            .copied()
            .ok_or(Error::NotFound),
        _ => Err(Error::InvalidArgument),
    };
    enum_adapter_iid(env, adapter, env.arg(3), env.arg(4))
}

fn enum_outputs(env: &mut Env<'_>) -> Outcome {
    let (this, index, output) = (env.arg(0), env.arg(1), env.arg(2));
    interface_out(env, output, |env| ops::enum_output(env, this, index))
}

fn software_adapter(env: &mut Env<'_>) -> Outcome {
    // Custom software rasterizer DLLs are excluded. WARP uses EnumWarpAdapter.
    let module = env.arg(1);
    interface_out(env, env.arg(2), |env| {
        Err(if module == 0 {
            Error::InvalidCall
        } else {
            adv_abi::warn_unavailable(
                env,
                "IDXGIFactory::CreateSoftwareAdapter",
                "guest software rasterizer DLLs are outside the adapter set",
            );
            Error::Unsupported
        })
    })
}

fn associate_window(env: &mut Env<'_>) -> Outcome {
    let (this, window, flags) = (env.arg(0), env.arg(1), env.arg(2));
    call(|| ops::associate_window(env, this, window, flags))
}

fn window(env: &mut Env<'_>) -> Outcome {
    call(|| write(env, env.arg(1), ops::window(env, env.arg(0))?))
}

fn yes(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(1)
}

fn zero(_: &mut Env<'_>) -> Outcome {
    Outcome::Return(0)
}

fn factory_feature(env: &mut Env<'_>) -> Outcome {
    call(|| {
        if env.arg(1) != 0 || env.arg(3) != 4 {
            return Err(Error::InvalidArgument);
        }
        write(env, env.arg(2), 0u32)
    })
}

fn check_interface(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let version = ops::interface_support(env, env.arg(0), read(env, env.arg(1))?)?;
        let output = env.arg(2);
        if output != 0 {
            write(env, output, version)?;
        }
        Ok(())
    })
}

fn adapter_desc_prefix<T: Pod>(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let desc = ops::adapter_desc(env, env.arg(0))?;
        let prefix = &bytemuck::bytes_of(&desc)[..size_of::<T>()];
        write(env, env.arg(1), bytemuck::pod_read_unaligned::<T>(prefix))
    })
}

fn adapter_desc(env: &mut Env<'_>) -> Outcome {
    adapter_desc_prefix::<DXGI_ADAPTER_DESC>(env)
}
fn adapter_desc1(env: &mut Env<'_>) -> Outcome {
    adapter_desc_prefix::<DXGI_ADAPTER_DESC1>(env)
}
fn adapter_desc2(env: &mut Env<'_>) -> Outcome {
    adapter_desc_prefix::<DXGI_ADAPTER_DESC2>(env)
}
fn adapter_desc3(env: &mut Env<'_>) -> Outcome {
    adapter_desc_prefix::<DXGI_ADAPTER_DESC3>(env)
}

fn video_memory(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let info = ops::video_memory(env, env.arg(0), env.arg(1), env.arg(2))?;
        write(env, env.arg(3), info)
    })
}

fn reserve_memory(env: &mut Env<'_>) -> Outcome {
    call(|| {
        ops::reserve_memory(
            env,
            env.arg(0),
            env.arg(1),
            env.arg(2),
            u64::from(env.arg(3)) | (u64::from(env.arg(4)) << 32),
        )
    })
}

fn output_desc(env: &mut Env<'_>) -> Outcome {
    call(|| {
        write(
            env,
            env.arg(1),
            ops::output_display(env, env.arg(0))?.description(),
        )
    })
}

fn output_desc1(env: &mut Env<'_>) -> Outcome {
    call(|| {
        write(
            env,
            env.arg(1),
            ops::output_display(env, env.arg(0))?.description1(),
        )
    })
}

fn mode_list(env: &mut Env<'_>) -> Outcome {
    list(env, false)
}
fn mode_list1(env: &mut Env<'_>) -> Outcome {
    list(env, true)
}

fn list(env: &mut Env<'_>, v1: bool) -> Outcome {
    call(|| {
        let (count, output) = (env.arg(3), env.arg(4));
        let request = ModeRequest::new(env.arg(1) as i32, env.arg(2), v1)?;
        let modes = ops::mode_list(env, env.arg(0), request)?;
        let needed = modes.len() as u32;
        let capacity = if output == 0 { 0u32 } else { read(env, count)? };
        write(env, count, needed)?;
        if output == 0 {
            return Ok(());
        }
        if capacity < needed {
            return Err(Error::MoreData);
        }
        if v1 {
            write_modes(env, output, &modes)
        } else {
            let modes = modes.into_iter().map(desc::base_mode).collect::<Vec<_>>();
            write_modes(env, output, &modes)
        }
    })
}

fn write_modes<T: Pod>(env: &mut Env<'_>, output: u32, modes: &[T]) -> Result<(), Error> {
    if modes.is_empty() {
        return Ok(());
    }
    let bytes = bytemuck::cast_slice(modes);
    validate_output(env, output, bytes.len() as u32)?;
    env.cpu.write(output, bytes).map_err(|_| Error::InvalidCall)
}

fn closest_mode(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let input: DXGI_MODE_DESC = read(env, env.arg(1))?;
        let mode = DXGI_MODE_DESC1 {
            Width: input.Width,
            Height: input.Height,
            RefreshRate: input.RefreshRate,
            Format: input.Format,
            ScanlineOrdering: input.ScanlineOrdering,
            Scaling: input.Scaling,
            Stereo: 0,
        };
        let matched = ops::closest(env, env.arg(0), desc::closest_request(mode)?)?;
        write(env, env.arg(2), desc::base_mode(matched))
    })
}

fn closest_mode1(env: &mut Env<'_>) -> Outcome {
    call(|| {
        let request = desc::closest_request(read(env, env.arg(1))?)?;
        write(env, env.arg(2), ops::closest(env, env.arg(0), request)?)
    })
}

fn wait_vblank(env: &mut Env<'_>) -> Outcome {
    ops::wait_vblank(env, env.arg(0))
}

fn frame_statistics(env: &mut Env<'_>) -> Outcome {
    call(|| {
        validate_output(env, env.arg(1), size_of::<DXGI_FRAME_STATISTICS>() as u32)?;
        Err(Error::FrameStatisticsDisjoint)
    })
}

fn composition_support(env: &mut Env<'_>) -> Outcome {
    call(|| write(env, env.arg(1), 0u32))
}

fn overlay(env: &mut Env<'_>) -> Outcome {
    overlay_result(env, 2, 3)
}
fn overlay_color_space(env: &mut Env<'_>) -> Outcome {
    overlay_result(env, 3, 4)
}

fn overlay_result(env: &mut Env<'_>, device: u32, flags: u32) -> Outcome {
    call(|| {
        validate_output(env, env.arg(flags), 4)?;
        if env.arg(device) == 0 {
            return Err(Error::InvalidArgument);
        }
        Err(Error::Unsupported)
    })
}

macro_rules! notifications {
    ($register:ident, $unregister:ident, $kind:ident, $window:literal, $cookie:literal, $void:literal) => {
        fn $register(env: &mut Env<'_>) -> Outcome {
            register_notification(env, Notification::$kind, $window, $cookie)
        }
        fn $unregister(env: &mut Env<'_>) -> Outcome {
            let result =
                ops::unregister_notification(env, env.arg(0), Notification::$kind, env.arg(1));
            if $void {
                Outcome::Return(0)
            } else {
                call(|| result)
            }
        }
    };
}
notifications!(
    register_stereo_event,
    unregister_stereo,
    Stereo,
    false,
    2,
    true
);
notifications!(
    register_occlusion_event,
    unregister_occlusion,
    Occlusion,
    false,
    2,
    true
);
notifications!(
    register_adapters,
    unregister_adapters,
    Adapters,
    false,
    2,
    false
);
notifications!(
    register_content_protection,
    unregister_content_protection,
    ContentProtection,
    false,
    2,
    true
);
notifications!(
    register_budget,
    unregister_budget,
    MemoryBudget,
    false,
    2,
    true
);

fn register_stereo_window(env: &mut Env<'_>) -> Outcome {
    register_notification(env, Notification::Stereo, true, 3)
}

fn register_occlusion_window(env: &mut Env<'_>) -> Outcome {
    register_notification(env, Notification::Occlusion, true, 3)
}

fn register_notification(
    env: &mut Env<'_>,
    kind: Notification,
    window: bool,
    cookie: u32,
) -> Outcome {
    call(|| {
        let output = env.arg(cookie);
        validate_output(env, output, 4)?;
        let cookie = ops::register_notification(env, env.arg(0), kind, env.arg(1), window)?;
        write(env, output, cookie)
    })
}

macro_rules! device_glue {
    ($handler:ident, $operation:ident, $argc:literal $(, $validate:expr)?) => {
        fn $handler(env: &mut Env<'_>) -> Outcome {
            $(if let Err(error) = ($validate)(env) {
                return Outcome::Return(error.hresult());
            })?
            let mut arguments = [0; 6];
            for (index, argument) in arguments.iter_mut().take($argc).enumerate() {
                *argument = env.arg(index as u32 + 1);
            }
            let invocation = DeviceCall {
                operation: DeviceOperation::$operation,
                this: env.arg(0),
                arguments,
            };
            if env.ext.try_get::<super::swap::Services>().is_some()
                && let Some(outcome) = swap_chain::create(env, invocation)
            {
                return outcome;
            }
            ops::device(env, invocation)
        }
    };
}

fn device_required(env: &Env<'_>, slot: u32) -> Result<(), Error> {
    if env.arg(slot) == 0 {
        Err(Error::InvalidArgument)
    } else {
        Ok(())
    }
}
fn structure<T: Pod>(env: &Env<'_>, slot: u32) -> Result<(), Error> {
    adv_abi::guest::check_range(env.cpu, env.arg(slot), size_of::<T>() as u32, Prot::READ)
        .map_err(|_| Error::InvalidCall)
}
fn validate_output(env: &Env<'_>, pointer: u32, bytes: u32) -> Result<(), Error> {
    adv_abi::guest::check_range(env.cpu, pointer, bytes, Prot::WRITE)
        .map_err(|_| Error::InvalidCall)
}
fn result_pointer<T: Pod>(env: &Env<'_>, slot: u32) -> Result<(), Error> {
    validate_output(env, env.arg(slot), size_of::<T>() as u32)
}
fn interface_pointer(env: &mut Env<'_>, slot: u32) -> Result<(), Error> {
    write(env, env.arg(slot), 0u32)
}

device_glue!(create_swap, CreateSwapChain, 3, |env: &mut Env<'_>| {
    interface_pointer(env, 3)?;
    device_required(env, 1)?;
    structure::<DXGI_SWAP_CHAIN_DESC>(env, 2)
});
device_glue!(
    create_swap_hwnd,
    CreateSwapChainForHwnd,
    6,
    |env: &mut Env<'_>| {
        interface_pointer(env, 6)?;
        device_required(env, 1)?;
        device_required(env, 2)?;
        if env.arg(4) != 0 {
            structure::<DXGI_SWAP_CHAIN_FULLSCREEN_DESC>(env, 4)?;
        }
        structure::<DXGI_SWAP_CHAIN_DESC1>(env, 3)
    }
);
device_glue!(
    create_swap_core,
    CreateSwapChainForCoreWindow,
    5,
    |env: &mut Env<'_>| {
        interface_pointer(env, 5)?;
        device_required(env, 1)?;
        structure::<DXGI_SWAP_CHAIN_DESC1>(env, 3)
    }
);
device_glue!(
    create_swap_composition,
    CreateSwapChainForComposition,
    4,
    |env: &mut Env<'_>| {
        interface_pointer(env, 4)?;
        device_required(env, 1)?;
        structure::<DXGI_SWAP_CHAIN_DESC1>(env, 2)
    }
);
fn shared_resource_luid(env: &mut Env<'_>) -> Outcome {
    let (this, handle, out) = (env.arg(0), env.arg(1), env.arg(2));
    if let Err(error) = result_pointer::<LUID>(env, 2) {
        return Outcome::Return(error.hresult());
    }
    if handle == 0 {
        return Outcome::Return(Error::InvalidCall.hresult());
    }
    let Some(services) = env.ext.try_get::<super::swap::Services>() else {
        return ops::device(
            env,
            DeviceCall {
                operation: DeviceOperation::GetSharedResourceAdapterLuid,
                this,
                arguments: [handle, out, 0, 0, 0, 0],
            },
        );
    };
    call(|| {
        let adapter = services.buffers.shared_adapter(env, handle)?;
        let info = &env.ext.get::<State>().provider.adapter;
        write(env, out, adapter.description(info).AdapterLuid)
    })
}
device_glue!(take_ownership, TakeOwnership, 2);
device_glue!(release_ownership, ReleaseOwnership, 0);
device_glue!(
    gamma_caps,
    GetGammaControlCapabilities,
    1,
    |env: &mut Env<'_>| result_pointer::<DXGI_GAMMA_CONTROL_CAPABILITIES>(env, 1)
);
device_glue!(
    set_gamma,
    SetGammaControl,
    1,
    |env: &mut Env<'_>| structure::<DXGI_GAMMA_CONTROL>(env, 1)
);
device_glue!(get_gamma, GetGammaControl, 1, |env: &mut Env<'_>| {
    result_pointer::<DXGI_GAMMA_CONTROL>(env, 1)
});
device_glue!(set_surface, SetDisplaySurface, 1, |env: &mut Env<'_>| {
    device_required(env, 1)
});
device_glue!(get_surface, GetDisplaySurfaceData, 1, |env: &mut Env<
    '_,
>| device_required(
    env, 1
));
device_glue!(get_surface1, GetDisplaySurfaceData1, 1, |env: &mut Env<
    '_,
>| device_required(
    env, 1
));
device_glue!(duplicate, DuplicateOutput, 2, |env: &mut Env<'_>| {
    interface_pointer(env, 2)?;
    device_required(env, 1)
});
device_glue!(duplicate1, DuplicateOutput1, 5, |env: &mut Env<'_>| {
    interface_pointer(env, 5)?;
    device_required(env, 1)?;
    let bytes = env.arg(3).checked_mul(4).ok_or(Error::InvalidArgument)?;
    adv_abi::guest::check_range(env.cpu, env.arg(4), bytes, Prot::READ)
        .map_err(|_| Error::InvalidArgument)
});

#[cfg(test)]
mod tests;

#[cfg(feature = "gpu")]
pub(crate) fn set_adapter(ext: &mut adv_machine::Ext, adapter: super::caps::AdapterInfo) {
    ext.get_mut::<State>().provider.adapter = adapter;
}

/// Install the window and device services for swap-chain callers.
pub fn install_swap_chains<B: Backend>(machine: &mut Machine<B>, services: super::swap::Services) {
    machine.ext.insert(services);
    machine
        .ext
        .get_mut::<State>()
        .provider
        .devices
        .get_or_insert(&output::OutputDevices);
}

pub fn has_swap_chain(ext: &adv_machine::Ext, window: u32) -> bool {
    ext.try_get::<State>()
        .is_some_and(|state| state.chains.contains_key(&window))
}

pub use swap_chain::alt_enter;

pub fn device_output(env: &mut Env<'_>, adapter: u32) -> Result<u32, Error> {
    ops::enum_output(env, adapter, 0)
}

pub fn create_device_swap_chain(
    env: &mut Env<'_>,
    adapter: u32,
    device: u32,
    desc: u32,
    output: u32,
) -> Result<(), Error> {
    let factory = ops::parent(env, adapter, IID_IDXGIFACTORY1)?;
    let result = swap_chain::create_legacy(env, factory, device, desc, output);
    ops::release(env, factory);
    result
}
