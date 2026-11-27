//! Factory, adapter and output method semantics.
use super::caps::{ADAPTERS, Adapter, Display};
use super::desc::{self, ModeRequest};
use super::object::{Kind, Notification, Object, PrivateData};
use super::state::{DeviceCall, DeviceOperation, Factory, Owner, State};
use super::{Error, abi::*};
use adv_abi::com::{IUNKNOWN, Iid, QueryError};
use adv_abi::{Env, Outcome};
use adv_cpu::backend::GuestExt;
use adv_machine::Wait;

impl From<QueryError> for Error {
    fn from(error: QueryError) -> Self {
        match error {
            QueryError::InvalidObject => Self::InvalidCall,
            QueryError::NoInterface => Self::NoInterface,
            QueryError::ReferenceLimit => Self::OutOfMemory,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FactoryOrigin {
    Legacy,
    Version1,
    Device,
}

impl FactoryOrigin {
    fn interfaces(self) -> impl Iterator<Item = Iid> {
        [
            (IID_IDXGIFACTORY, true),
            (IID_IDXGIFACTORY1, self != Self::Legacy),
            (IID_IDXGIFACTORY2, true),
            (IID_IDXGIFACTORY3, self == Self::Device),
            (IID_IDXGIFACTORY4, self == Self::Device),
            (IID_IDXGIFACTORY5, self == Self::Device),
            (IID_IDXGIFACTORY6, self == Self::Device),
            (IID_IDXGIFACTORY7, self == Self::Device),
        ]
        .into_iter()
        .filter_map(|(iid, enabled)| enabled.then_some(iid))
    }
}

pub(super) const ADAPTER_INTERFACES: [Iid; 5] = [
    IID_IDXGIADAPTER,
    IID_IDXGIADAPTER1,
    IID_IDXGIADAPTER2,
    IID_IDXGIADAPTER3,
    IID_IDXGIADAPTER4,
];
const OUTPUT_INTERFACES: [Iid; 7] = [
    IID_IDXGIOUTPUT,
    IID_IDXGIOUTPUT1,
    IID_IDXGIOUTPUT2,
    IID_IDXGIOUTPUT3,
    IID_IDXGIOUTPUT4,
    IID_IDXGIOUTPUT5,
    IID_IDXGIOUTPUT6,
];

pub(super) fn create_factory(
    env: &mut Env<'_>,
    origin: FactoryOrigin,
    iid: Iid,
) -> Result<u32, Error> {
    if !matches!(iid, IUNKNOWN | IID_IDXGIOBJECT)
        && !origin.interfaces().any(|supported| supported == iid)
    {
        return Err(Error::NoInterface);
    }
    let vtable = env.ext.get::<State>().factory_vtable;
    create(
        env,
        vtable,
        Kind::Factory(Factory::default()),
        None,
        origin.interfaces(),
    )
}

pub(super) fn create(
    env: &mut Env<'_>,
    vtable: u32,
    kind: Kind,
    parent: Option<u32>,
    interfaces: impl IntoIterator<Item = Iid>,
) -> Result<u32, Error> {
    let com = env.ext.get::<State>().com;
    let heaps = env.ext.get_mut::<adv_os::Heaps>();
    let pointer = heaps
        .alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, 4, false)
        .ok_or(Error::OutOfMemory)?;
    let object = Object {
        kind,
        parent,
        private: Default::default(),
        notifications: Default::default(),
        next_cookie: 1,
    };
    if env.cpu.write_u32(pointer, vtable).is_err() {
        free(env, pointer);
        return Err(Error::OutOfMemory);
    }
    let Some(identity) = com.create(&mut env.sys.objects, pointer, IID_IDXGIOBJECT, object) else {
        free(env, pointer);
        return Err(Error::OutOfMemory);
    };
    for iid in interfaces {
        assert!(com.register_interface(&mut env.sys.objects, identity, iid, pointer));
    }
    if let Some(parent) = parent {
        assert!(
            com.add_ref::<Object>(&mut env.sys.objects, parent)
                .is_some()
        );
    }
    Ok(pointer)
}

fn free(env: &mut Env<'_>, pointer: u32) {
    let heaps = env.ext.get_mut::<adv_os::Heaps>();
    heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
}

pub(super) fn object<'a>(env: &'a Env<'_>, this: u32) -> Result<&'a Object, Error> {
    env.ext
        .get::<State>()
        .com
        .get(&env.sys.objects, this)
        .ok_or(Error::InvalidCall)
}

pub(super) fn object_mut<'a>(env: &'a mut Env<'_>, this: u32) -> Result<&'a mut Object, Error> {
    env.ext
        .get::<State>()
        .com
        .get_mut(&mut env.sys.objects, this)
        .ok_or(Error::InvalidCall)
}

pub(super) fn query(env: &mut Env<'_>, this: u32, iid: Iid) -> Result<u32, Error> {
    Ok(env
        .ext
        .get::<State>()
        .com
        .query::<Object>(&mut env.sys.objects, this, iid)?)
}

pub(super) fn parent(env: &mut Env<'_>, this: u32, iid: Iid) -> Result<u32, Error> {
    let parent = object(env, this)?.parent.ok_or(Error::NoInterface)?;
    query(env, parent, iid)
}

pub(super) fn add_ref(env: &mut Env<'_>, this: u32) -> u32 {
    env.ext
        .get::<State>()
        .com
        .add_ref::<Object>(&mut env.sys.objects, this)
        .unwrap_or(0)
}

pub(super) fn release(env: &mut Env<'_>, this: u32) -> Outcome {
    let mut interfaces = Vec::new();
    let refs = destroy(env, this, &mut interfaces);
    release_interfaces(env, interfaces, refs)
}

fn destroy(env: &mut Env<'_>, this: u32, interfaces: &mut Vec<u32>) -> u32 {
    let com = env.ext.get::<State>().com;
    let Some(released) = com.release::<Object>(&mut env.sys.objects, this) else {
        return 0;
    };
    if let Some(destroyed) = released.destroyed {
        for pointer in destroyed.interfaces {
            free(env, pointer);
        }
        interfaces.extend(
            destroyed
                .value
                .private
                .into_values()
                .filter_map(|value| match value {
                    PrivateData::Interface(pointer) => Some(pointer),
                    PrivateData::Bytes(_) => None,
                }),
        );
        match destroyed.value.kind {
            Kind::SwapChain(chain) => {
                if chain.fullscreen.Windowed == 0 {
                    release_output(env, this, interfaces);
                }
                chain.destroy(env, this, interfaces);
            }
            Kind::Duplication(duplication) => {
                env.ext.get_mut::<State>().output.duplicating = false;
                interfaces.push(duplication.device);
            }
            Kind::Output => release_output(env, this, interfaces),
            Kind::Factory(_) | Kind::Adapter(_) => (),
        }
        if let Some(parent) = destroyed.value.parent {
            destroy(env, parent, interfaces);
        }
    }
    released.references
}

pub(super) fn release_interfaces(
    env: &mut Env<'_>,
    mut interfaces: Vec<u32>,
    result: u32,
) -> Outcome {
    match interfaces.pop() {
        Some(pointer) => unknown_call(env, pointer, 2, move |env| {
            release_interfaces(env, interfaces, result)
        }),
        None => Outcome::Return(result),
    }
}

fn unknown_call(
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
        return Outcome::Return(Error::InvalidCall.hresult());
    };
    Outcome::call(target, [pointer], move |env, _| then(env))
}

pub(super) fn store_private(
    env: &mut Env<'_>,
    this: u32,
    iid: Iid,
    value: Option<PrivateData>,
) -> Outcome {
    let Ok(object) = object_mut(env, this) else {
        return match value {
            Some(PrivateData::Interface(pointer)) => unknown_call(env, pointer, 2, |_| {
                Outcome::Return(Error::InvalidCall.hresult())
            }),
            Some(PrivateData::Bytes(_)) | None => Outcome::Return(Error::InvalidCall.hresult()),
        };
    };
    let old = match value {
        Some(value) => object.private.insert(iid, value),
        None => {
            let Some(old) = object.private.remove(&iid) else {
                return Outcome::Return(1);
            };
            Some(old)
        }
    };
    match old {
        Some(PrivateData::Interface(pointer)) => {
            unknown_call(env, pointer, 2, |_| Outcome::Return(0))
        }
        Some(PrivateData::Bytes(_)) | None => Outcome::Return(0),
    }
}

pub(super) fn store_interface(env: &mut Env<'_>, this: u32, iid: Iid, pointer: u32) -> Outcome {
    if pointer == 0 {
        return store_private(env, this, iid, None);
    }
    if let Err(error) = object(env, this) {
        return Outcome::Return(error.hresult());
    }
    unknown_call(env, pointer, 1, move |env| {
        store_private(env, this, iid, Some(PrivateData::Interface(pointer)))
    })
}

pub(super) fn private(env: &Env<'_>, this: u32, iid: Iid) -> Result<PrivateData, Error> {
    match object(env, this)?
        .private
        .get(&iid)
        .ok_or(Error::NotFound)?
    {
        PrivateData::Bytes(bytes) => Ok(PrivateData::Bytes(bytes.clone())),
        PrivateData::Interface(pointer) => Ok(PrivateData::Interface(*pointer)),
    }
}

pub(super) fn retain_private(env: &mut Env<'_>, pointer: u32) -> Outcome {
    unknown_call(env, pointer, 1, |_| Outcome::Return(0))
}

pub(super) fn enum_adapter(
    env: &mut Env<'_>,
    factory: u32,
    adapter: Adapter,
) -> Result<u32, Error> {
    if !matches!(object(env, factory)?.kind, Kind::Factory(_)) {
        return Err(Error::InvalidCall);
    }
    let vtable = env.ext.get::<State>().adapter_vtable;
    create(
        env,
        vtable,
        Kind::Adapter(adapter),
        Some(factory),
        ADAPTER_INTERFACES,
    )
}

pub(super) fn enum_output(env: &mut Env<'_>, this: u32, index: u32) -> Result<u32, Error> {
    if index != 0 || adapter(env, this)? != Adapter::Hardware || display(env).is_none() {
        return Err(Error::NotFound);
    }
    let vtable = env.ext.get::<State>().output_vtable;
    create(env, vtable, Kind::Output, Some(this), OUTPUT_INTERFACES)
}

pub(super) fn adapter(env: &Env<'_>, this: u32) -> Result<Adapter, Error> {
    match object(env, this)?.kind {
        Kind::Adapter(adapter) => Ok(adapter),
        Kind::Factory(_) | Kind::Output | Kind::SwapChain(_) | Kind::Duplication(_) => {
            Err(Error::InvalidCall)
        }
    }
}

pub(super) fn adapter_desc(env: &Env<'_>, this: u32) -> Result<DXGI_ADAPTER_DESC2, Error> {
    Ok(adapter(env, this)?.description(&env.ext.get::<State>().provider.adapter))
}

pub(super) fn adapter_by_luid(env: &Env<'_>, luid: u64) -> Result<Adapter, Error> {
    let adapter_info = &env.ext.get::<State>().provider.adapter;
    ADAPTERS
        .into_iter()
        .find(|adapter| {
            let candidate = adapter.description(adapter_info).AdapterLuid;
            u64::from(candidate.LowPart) | (u64::from(candidate.HighPart as u32) << 32) == luid
        })
        .ok_or(Error::NotFound)
}

pub(super) fn interface_support(env: &Env<'_>, this: u32, iid: Iid) -> Result<u64, Error> {
    let adapter = adapter(env, this)?;
    if !matches!(iid, IID_IDXGIDEVICE | IID_ID3D10DEVICE) {
        return Err(Error::Unsupported);
    }
    Ok(match adapter {
        Adapter::Hardware => env.ext.get::<State>().provider.adapter.driver_version,
        Adapter::Software | Adapter::Null => 0,
    })
}

pub(super) fn associate_window(
    env: &mut Env<'_>,
    this: u32,
    window: u32,
    flags: u32,
) -> Result<(), Error> {
    if flags & !7 != 0
        || (window == 0 && flags != 0)
        || (window != 0
            && !(env.ext.get::<State>().provider.window_exists)(env.ext, &env.sys.objects, window))
    {
        return Err(Error::InvalidCall);
    }
    let Kind::Factory(factory) = &mut object_mut(env, this)?.kind else {
        return Err(Error::InvalidCall);
    };
    factory.window = Some(window);
    factory.window_flags = flags;
    Ok(())
}

pub(super) fn window(env: &Env<'_>, this: u32) -> Result<u32, Error> {
    let Kind::Factory(factory) = &object(env, this)?.kind else {
        return Err(Error::InvalidCall);
    };
    Ok(factory.window.unwrap_or_default())
}

fn display(env: &Env<'_>) -> Option<Display> {
    (env.ext.get::<State>().provider.display)(env.ext, env.context)
}

pub(super) fn output_display(env: &Env<'_>, this: u32) -> Result<Display, Error> {
    if !matches!(object(env, this)?.kind, Kind::Output) {
        return Err(Error::InvalidCall);
    }
    display(env).ok_or(Error::NotFound)
}

pub(super) fn mode_list(
    env: &Env<'_>,
    this: u32,
    request: ModeRequest,
) -> Result<Vec<DXGI_MODE_DESC1>, Error> {
    Ok(output_display(env, this)?
        .modes
        .into_iter()
        .filter(|mode| request.includes(mode))
        .map(|mode| desc::mode_desc(mode, request.format))
        .collect())
}

pub(super) fn closest(
    env: &Env<'_>,
    this: u32,
    request: DXGI_MODE_DESC1,
) -> Result<DXGI_MODE_DESC1, Error> {
    let display = output_display(env, this)?;
    let [left, top, right, bottom] = display.physical_rect;
    let (width, height) = if request.Width == 0 {
        ((right - left) as u32, (bottom - top) as u32)
    } else {
        (request.Width, request.Height)
    };
    let refresh = |r: DXGI_RATIONAL| f64::from(r.Numerator) / f64::from(r.Denominator);
    display
        .modes
        .into_iter()
        .filter(|mode| matches!(request.Format, 28 | 87) && mode.stereo == (request.Stereo != 0))
        .map(|mode| desc::mode_desc(mode, request.Format))
        .min_by_key(|mode| {
            let distance =
                u64::from(mode.Width.abs_diff(width)) + u64::from(mode.Height.abs_diff(height));
            let scanline =
                request.ScanlineOrdering != 0 && mode.ScanlineOrdering != request.ScanlineOrdering;
            let scaling = request.Scaling != 0 && mode.Scaling != request.Scaling;
            let hz = if request.RefreshRate.Denominator == 0 {
                0.0
            } else {
                (refresh(mode.RefreshRate) - refresh(request.RefreshRate)).abs()
            };
            (distance, scanline, scaling, hz.to_bits())
        })
        .ok_or(Error::NotFound)
}

pub(super) fn wait_vblank(env: &Env<'_>, this: u32) -> Outcome {
    if let Err(error) = output_display(env, this) {
        return Outcome::Return(error.hresult());
    }
    match (env.ext.get::<State>().provider.next_vblank)(env.ext) {
        Some(delay) => Outcome::block_then_return(
            Wait::Until(env.sys.clock.now_us() + delay.as_micros() as u64),
            0,
        ),
        None => Outcome::Return(0),
    }
}

pub(super) fn video_memory(
    env: &Env<'_>,
    this: u32,
    node: u32,
    segment: u32,
) -> Result<DXGI_QUERY_VIDEO_MEMORY_INFO, Error> {
    adapter(env, this)?;
    if node != 0 || segment > 1 {
        return Err(Error::InvalidCall);
    }
    // The adapter's descriptive memory figures are not enforceable budgets.
    Ok(DXGI_QUERY_VIDEO_MEMORY_INFO::default())
}

pub(super) fn reserve_memory(
    env: &Env<'_>,
    this: u32,
    node: u32,
    segment: u32,
    amount: u64,
) -> Result<(), Error> {
    let memory = video_memory(env, this, node, segment)?;
    if amount > memory.AvailableForReservation {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

pub(super) fn register_notification(
    env: &mut Env<'_>,
    this: u32,
    notification: Notification,
    handle: u32,
    window: bool,
) -> Result<u32, Error> {
    let valid = if window {
        (env.ext.get::<State>().provider.window_exists)(env.ext, &env.sys.objects, handle)
    } else {
        matches!(
            env.sys.objects.get(handle),
            Some(adv_machine::Object::Event { .. })
        )
    };
    if !valid {
        return Err(Error::InvalidCall);
    }
    let object = object_mut(env, this)?;
    let cookie = object.next_cookie;
    object.next_cookie += 1;
    object.notifications.insert(cookie, notification);
    // Adapter, memory, stereo and occlusion facts are fixed by the current virtual
    // profile. These subscriptions have no transitions to signal in this front.
    Ok(cookie)
}

pub(super) fn unregister_notification(
    env: &mut Env<'_>,
    this: u32,
    notification: Notification,
    cookie: u32,
) -> Result<(), Error> {
    let object = object_mut(env, this)?;
    if object.notifications.get(&cookie) != Some(&notification) {
        return Err(Error::NotFound);
    }
    object.notifications.remove(&cookie);
    Ok(())
}

pub(super) fn claim_output(
    env: &mut Env<'_>,
    claimant: u32,
    device: u32,
    direct: bool,
) -> Result<(), Error> {
    if let Some(owner) = &env.ext.get::<State>().output.owner {
        return if owner.claimant == claimant || direct && owner.device == device {
            Ok(())
        } else {
            Err(Error::Unavailable)
        };
    }
    let buffers = env.ext.get::<super::swap::Services>().buffers;
    if direct {
        buffers.retain_device(env, device)?;
    }
    if !crate::ddraw::front::modes::set_modern_exclusive(env.ext, claimant, true) {
        if direct {
            buffers.drop_device_ref(env, device);
        }
        return Err(Error::Unavailable);
    }
    let output = &mut env.ext.get_mut::<State>().output;
    output.owner = Some(Owner {
        claimant,
        device,
        retained: direct,
    });
    output.epoch += 1;
    Ok(())
}

pub(super) fn release_output(env: &mut Env<'_>, claimant: u32, interfaces: &mut Vec<u32>) {
    let output = &mut env.ext.get_mut::<State>().output;
    if output
        .owner
        .as_ref()
        .is_some_and(|owner| owner.claimant == claimant)
    {
        let owner = output.owner.take().unwrap();
        output.gamma = None;
        output.epoch += 1;
        crate::ddraw::front::modes::set_modern_exclusive(env.ext, claimant, false);
        if owner.retained {
            interfaces.push(owner.device);
        }
    }
}

pub(super) fn output_mode_changed(ext: &mut adv_machine::Ext) {
    ext.get_mut::<State>().output.epoch += 1;
}

pub(super) fn device(env: &mut Env<'_>, call: DeviceCall) -> Outcome {
    if let Some(devices) = env.ext.get::<State>().provider.devices {
        return devices.invoke(env, call);
    }
    device_unavailable(call.operation)
}

pub(super) fn device_unavailable(operation: DeviceOperation) -> Outcome {
    let error = match operation {
        DeviceOperation::CreateSwapChain
        | DeviceOperation::CreateSwapChainForHwnd
        | DeviceOperation::CreateSwapChainForCoreWindow
        | DeviceOperation::CreateSwapChainForComposition
        | DeviceOperation::DuplicateOutput
        | DeviceOperation::DuplicateOutput1 => Error::InvalidArgument,
        DeviceOperation::GetSharedResourceAdapterLuid => Error::InvalidCall,
        DeviceOperation::ReleaseOwnership => return Outcome::Return(0),
        DeviceOperation::TakeOwnership
        | DeviceOperation::GetGammaControlCapabilities
        | DeviceOperation::SetGammaControl
        | DeviceOperation::GetGammaControl
        | DeviceOperation::SetDisplaySurface
        | DeviceOperation::GetDisplaySurfaceData
        | DeviceOperation::GetDisplaySurfaceData1 => Error::Unavailable,
    };
    Outcome::Return(error.hresult())
}
